use ytm_control_api::{
    ControlError, ErrorCode, GrantAction, OperationPhase, OperationResource, RetryDisposition,
    VerificationLevel, WriteAction,
};
use ytm_protocol::ArtistId;

use super::catalog::CatalogWriteError;
use super::write::{MusicOperation, Transition};
use super::{invalid, map_query_error, stale, unavailable};

impl MusicOperation {
    pub(super) async fn set_artist_subscription(&mut self) -> Result<Transition, ControlError> {
        let WriteAction::SetArtistSubscription {
            artist_id,
            subscribed,
        } = self.input.action.clone()
        else {
            return Err(invalid("expected an artist subscription change"));
        };
        if self.state.snapshot.account_epoch.is_none()
            || self.input.preconditions.expected_account_epoch != self.state.snapshot.account_epoch
        {
            return Err(stale("a matching authenticated account epoch is required"));
        }
        let catalog = self
            .service
            .catalog
            .clone()
            .ok_or_else(|| unavailable("the music catalog is unavailable"))?;
        let before = catalog
            .artist_subscription(self.fresh_context().await?, artist_id.clone())
            .await
            .map_err(map_query_error)?;
        self.fresh_context().await?;
        let Some(existing) = before.value else {
            return Err(ControlError::new(
                ErrorCode::CapabilityUnavailable,
                RetryDisposition::NotRetryable,
                "the official page did not expose subscription state for this channel",
            ));
        };
        if existing == subscribed {
            self.current.resource = Some(OperationResource::Artist {
                id: artist_id,
                subscribed: Some(existing),
            });
            return Ok(Transition::Verified(VerificationLevel::ReadBack));
        }

        let mut submitted = self.current.clone();
        submitted.phase = OperationPhase::Submitted;
        submitted.revision += 1;
        submitted.resource = Some(OperationResource::Artist {
            id: artist_id.clone(),
            subscribed: None,
        });
        self.apply(submitted).await?;
        let context = match self.fresh_context().await {
            Ok(context) => context,
            Err(error) => return Ok(Transition::Rejected(error)),
        };
        if let Err(error) = self
            .authority
            .authorize(GrantAction::ArtistSubscription)
            .await
        {
            return Ok(Transition::Rejected(error));
        }
        if self.cancel.is_cancelled() {
            return Ok(Transition::Cancelled);
        }
        if let Err(CatalogWriteError::Rejected(error)) = catalog
            .set_artist_subscription(context, artist_id.clone(), subscribed)
            .await
        {
            return Ok(Transition::Rejected(error));
        }
        self.refresh_artist(&artist_id).await;
        let actual = catalog
            .artist_subscription(self.fresh_context().await?, artist_id.clone())
            .await;
        self.fresh_context().await?;
        if !actual.is_ok_and(|evidence| evidence.value == Some(subscribed)) {
            return Ok(Transition::Unknown(unavailable(
                "fresh YouTube Music data did not confirm the subscription",
            )));
        }
        self.current.resource = Some(OperationResource::Artist {
            id: artist_id,
            subscribed: Some(subscribed),
        });
        Ok(Transition::Verified(VerificationLevel::ReadBack))
    }

    /// Publishes refresh events for a channel whose subscription a sent write may have changed.
    pub(super) async fn refresh_artist(&self, artist_id: &ArtistId) {
        if let Some(epoch) = self.state.snapshot.account_epoch.clone() {
            let _ = self
                .service
                .core
                .invalidate_control_resource(
                    epoch.clone(),
                    "artist".into(),
                    artist_id.as_str().into(),
                )
                .await;
            let _ = self
                .service
                .core
                .invalidate_control_resource(epoch, "library".into(), "subscriptions".into())
                .await;
        }
    }
}
