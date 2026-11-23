use ytm_control_api::{
    ControlError, GrantAction, OperationPhase, OperationResource, Rating, TrackTarget,
    VerificationLevel, WriteAction,
};

use ytm_protocol::VideoId;

use super::catalog::{CatalogWriteError, ControlCatalog};
use super::write::{MusicOperation, Transition};
use super::{invalid, stale, unavailable};

impl MusicOperation {
    pub(super) async fn rate(&mut self) -> Result<Transition, ControlError> {
        let WriteAction::Rate { target, rating } = &self.input.action else {
            return Err(invalid("expected a rating change"));
        };
        let rating = *rating;
        if rating == Rating::Unknown {
            return Err(invalid("unverified is not a rating that can be set"));
        }
        let video_id = match target {
            TrackTarget::Video { video_id } => video_id.clone(),
            TrackTarget::Context { context_token } => self
                .service
                .resolve_context(&self.grant, context_token)
                .await?
                .video_id
                .ok_or_else(|| invalid("the context had no track"))?,
        };
        if self.state.snapshot.account_epoch.is_none()
            || self.input.preconditions.expected_account_epoch != self.state.snapshot.account_epoch
        {
            return Err(stale("a matching authenticated account epoch is required"));
        }
        self.fresh_context().await?;
        let catalog = self
            .service
            .catalog
            .clone()
            .ok_or_else(|| unavailable("the music catalog is unavailable"))?;
        let mut submitted = self.current.clone();
        submitted.revision += 1;
        submitted.phase = OperationPhase::Submitted;
        submitted.resource = Some(OperationResource::Track {
            id: video_id.clone(),
        });
        self.apply(submitted).await?;
        let context = match self.fresh_context().await {
            Ok(context) => context,
            Err(error) => return Ok(Transition::Rejected(error)),
        };
        if let Err(error) = self.authority.authorize(GrantAction::Rating).await {
            return Ok(Transition::Rejected(error));
        }
        if self.cancel.is_cancelled() {
            return Ok(Transition::Cancelled);
        }
        let acknowledgement = catalog.set_rating(context, video_id.clone(), rating).await;
        if let Err(CatalogWriteError::Rejected(error)) = acknowledgement {
            return Ok(Transition::Rejected(error));
        }
        self.refresh_rating(catalog.as_ref(), &video_id).await;
        let evidence = catalog
            .fresh_rating(self.fresh_context().await?, video_id.clone())
            .await;
        if !evidence.is_ok_and(|evidence| evidence.rating == rating) {
            return Ok(Transition::Unknown(unavailable(
                "fresh YouTube Music data did not confirm the requested rating",
            )));
        }
        Ok(Transition::Verified(VerificationLevel::ReadBack))
    }

    /// Retires rating-bearing caches and publishes refresh events after a rating write that
    /// may have reached the provider.
    pub(super) async fn refresh_rating(&self, catalog: &dyn ControlCatalog, video_id: &VideoId) {
        if let Some(scope) = self.grant.scope.clone()
            && let Err(error) = catalog.invalidate_rating_cache(scope).await
        {
            tracing::warn!(code = ?error.code, "rating changed but cached music pages could not be invalidated");
        }
        if let Some(epoch) = self.state.snapshot.account_epoch.clone() {
            for entity in ["rating", "likedSongs"] {
                let _ = self
                    .service
                    .core
                    .invalidate_control_resource(
                        epoch.clone(),
                        entity.into(),
                        video_id.as_str().into(),
                    )
                    .await;
            }
        }
    }
}
