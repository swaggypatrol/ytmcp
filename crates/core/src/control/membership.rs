use std::time::Duration;

use tokio::sync::OwnedMutexGuard;
use ytm_control_api::{
    ControlError, ErrorCode, GrantAction, LibraryMembershipTarget, MembershipEvidence,
    OperationPhase, OperationResource, RetryDisposition, VerificationLevel, WriteAction,
};
use ytm_protocol::{PlaylistId, QueryError};

use super::catalog::{CatalogReadContext, CatalogWriteError, ControlCatalog};
use super::catalog_locks::CatalogTarget;
use super::write::{MusicOperation, Transition, permission};
use super::{invalid, map_query_error, stale, unavailable};

impl MusicOperation {
    pub(super) async fn set_library_membership(&mut self) -> Result<Transition, ControlError> {
        let WriteAction::SetLibraryMembership { target, saved } = self.input.action.clone() else {
            return Err(invalid("expected a library membership change"));
        };
        if self.state.snapshot.account_epoch.is_none()
            || self.input.preconditions.expected_account_epoch != self.state.snapshot.account_epoch
        {
            return Err(stale("a matching authenticated account epoch is required"));
        }
        let Some(_guard) = self.lock_membership(&target).await? else {
            return Ok(Transition::Cancelled);
        };
        let catalog = self
            .service
            .catalog
            .clone()
            .ok_or_else(|| unavailable("the music catalog is unavailable"))?;
        let mut context = self.fresh_context().await?;
        context.cancel = self.cancel.clone();
        let (before, album_playlist_id) = read_membership(catalog.as_ref(), context, &target)
            .await
            .map_err(map_query_error)?;
        self.fresh_context().await?;
        let Some(existing) = before.value else {
            return Err(ControlError::new(
                ErrorCode::CapabilityUnavailable,
                RetryDisposition::NotRetryable,
                "YouTube Music did not expose exact membership for this item",
            ));
        };
        if self.cancel.is_cancelled() {
            return Ok(Transition::Cancelled);
        }
        if existing == saved {
            self.refresh_membership(catalog.as_ref(), &target).await;
            self.fresh_context().await?;
            self.current.resource = Some(membership_resource(
                target,
                Some(existing),
                album_playlist_id,
            ));
            return Ok(Transition::Verified(VerificationLevel::ReadBack));
        }

        let mut submitted = self.current.clone();
        submitted.phase = OperationPhase::Submitted;
        submitted.revision += 1;
        submitted.resource = Some(membership_resource(
            target.clone(),
            None,
            album_playlist_id.clone(),
        ));
        self.apply(submitted).await?;
        let mut context = match self.fresh_context().await {
            Ok(context) => context,
            Err(error) => return Ok(Transition::Rejected(error)),
        };
        context.cancel = self.cancel.clone();
        let grant = match self
            .authority
            .authorize(GrantAction::LibraryMembership)
            .await
        {
            Ok(grant) => grant,
            Err(error) => return Ok(Transition::Rejected(error)),
        };
        let remaining_ms = grant
            .deadline_wall_ms
            .saturating_sub(self.service.core.clock().wall_now_ms());
        if remaining_ms == 0 {
            return Ok(Transition::Rejected(permission(
                "the grant expired before dispatch",
            )));
        }
        if self.cancel.is_cancelled() {
            return Ok(Transition::Cancelled);
        }
        let apply = async {
            match &target {
                LibraryMembershipTarget::Album { id } => {
                    catalog
                        .set_album_membership(
                            context,
                            id.clone(),
                            album_playlist_id
                                .clone()
                                .expect("album read establishes a playlist identity"),
                            saved,
                        )
                        .await
                }
                _ => {
                    catalog
                        .set_library_membership(context, target.clone(), saved)
                        .await
                }
            }
        };
        tokio::select! {
            biased;
            _ = tokio::time::sleep(Duration::from_millis(remaining_ms)) => {},
            result = apply => {
                if let Err(CatalogWriteError::Rejected(error)) = result {
                    return Ok(Transition::Rejected(error));
                }
            }
        }
        let confirmed = async {
            let mut context = self.fresh_context().await?;
            context.cancel = self.cancel.clone();
            let actual = read_membership(catalog.as_ref(), context, &target).await;
            Ok::<_, ControlError>(actual.is_ok_and(|(observed, actual_album)| {
                observed.value == Some(saved) && actual_album == album_playlist_id
            }))
        }
        .await;
        self.refresh_membership(catalog.as_ref(), &target).await;
        self.fresh_context().await?;
        if !confirmed? {
            return Ok(Transition::Unknown(unavailable(
                "fresh YouTube Music data did not confirm the requested membership",
            )));
        }
        self.current.resource = Some(membership_resource(target, Some(saved), album_playlist_id));
        Ok(Transition::Verified(VerificationLevel::ReadBack))
    }

    pub(super) async fn lock_membership(
        &self,
        target: &LibraryMembershipTarget,
    ) -> Result<Option<OwnedMutexGuard<()>>, ControlError> {
        let scope = self
            .grant
            .scope
            .clone()
            .ok_or_else(|| permission("a matching account grant is required"))?;
        let target = match target {
            LibraryMembershipTarget::Song { id } => CatalogTarget::Song(id.clone()),
            LibraryMembershipTarget::Playlist { id } => CatalogTarget::Playlist(Some(id.clone())),
            LibraryMembershipTarget::Album { id } => CatalogTarget::Album(id.clone()),
        };
        let lock = self.service.catalog_locks.for_target(scope, target)?;
        Ok(tokio::select! {
            biased;
            _ = self.cancel.cancelled() => None,
            guard = lock.lock_owned() => Some(guard),
        })
    }

    pub(super) async fn refresh_membership(
        &self,
        catalog: &dyn ControlCatalog,
        target: &LibraryMembershipTarget,
    ) {
        let (Some(scope), Some(epoch)) = (
            self.grant.scope.clone(),
            self.state.snapshot.account_epoch.clone(),
        ) else {
            return;
        };
        if let Err(error) = catalog
            .invalidate_library_membership(scope, target.clone())
            .await
        {
            tracing::warn!(code=?error.code, "could not invalidate cached library membership");
        }
        let (kind, id) = match target {
            LibraryMembershipTarget::Song { id } => ("songMembership", id.as_str()),
            LibraryMembershipTarget::Playlist { id } => ("playlistMembership", id.as_str()),
            LibraryMembershipTarget::Album { id } => ("albumMembership", id.as_str()),
        };
        let _ = self
            .service
            .core
            .invalidate_control_resource(epoch, kind.into(), id.into())
            .await;
    }
}

pub(super) async fn read_membership(
    catalog: &dyn ControlCatalog,
    context: CatalogReadContext,
    target: &LibraryMembershipTarget,
) -> Result<(MembershipEvidence, Option<PlaylistId>), QueryError> {
    match target {
        LibraryMembershipTarget::Album { id } => catalog
            .album_membership(context, id.clone())
            .await
            .map(|(playlist_id, evidence)| (evidence, Some(playlist_id))),
        _ => catalog
            .library_membership(context, target.clone())
            .await
            .map(|evidence| (evidence, None)),
    }
}

fn membership_resource(
    target: LibraryMembershipTarget,
    saved: Option<bool>,
    album_playlist_id: Option<PlaylistId>,
) -> OperationResource {
    OperationResource::LibraryMembership {
        target,
        saved,
        album_playlist_id,
    }
}
