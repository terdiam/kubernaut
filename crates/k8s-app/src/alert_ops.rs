//! Alertmanager reads and silences, shared by every front end.
//!
//! Silencing changes what the team is paged about, so it is held to the same
//! rules as any other write: a protected cluster refuses it and the audit log
//! records it. Confirming with the user is the front end's job.

use std::time::Duration;

use k8s_metrics::{Alert, AlertmanagerTarget, SilenceRequest, alertmanager};
use k8s_openapi::jiff::{SignedDuration, Timestamp};

use crate::state::AppCore;

/// Who a silence says made it, so an on-call engineer can tell where it came from.
const CREATED_BY: &str = "Kubernaut";

/// The cluster's alerts and the Alertmanager they came from.
pub struct AlertSurvey {
    pub target: AlertmanagerTarget,
    pub alerts: Vec<Alert>,
}

/// Read the alerts, or `None` when the cluster has no Alertmanager.
///
/// Finding the Alertmanager lists every Service, so a caller that polls passes
/// back the target it already has.
pub async fn survey(
    core: &AppCore,
    cluster: &str,
    known: Option<AlertmanagerTarget>,
) -> Result<Option<AlertSurvey>, String> {
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let target = match known {
        Some(target) => target,
        None => match alertmanager::discover(&handle).await {
            Some(target) => target,
            None => return Ok(None),
        },
    };
    let alerts = alertmanager::list_alerts(&handle, &target).await?;
    Ok(Some(AlertSurvey { target, alerts }))
}

/// Silence every alert with this name for `duration`. Returns the silence's id.
pub async fn silence(
    core: &AppCore,
    cluster: &str,
    target: &AlertmanagerTarget,
    alertname: &str,
    duration: Duration,
) -> Result<String, String> {
    core.ensure_writable(cluster)?;
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let ends_at = SignedDuration::try_from(duration)
        .ok()
        .and_then(|duration| Timestamp::now().checked_add(duration).ok())
        .ok_or("That silence is too long.")?;
    let request = SilenceRequest {
        matcher_name: "alertname".into(),
        matcher_value: alertname.into(),
        ends_at: ends_at.to_string(),
        created_by: CREATED_BY.into(),
        comment: "Silenced from Kubernaut".into(),
    };
    let result = alertmanager::post_silence(&handle, target, &request).await;
    core.record_audit(cluster, "silenceAlert", alertname, &result);
    result
}

/// End silences early, so their alerts notify again. Returns how many ended.
pub async fn expire(
    core: &AppCore,
    cluster: &str,
    target: &AlertmanagerTarget,
    alertname: &str,
    silences: &[String],
) -> Result<usize, String> {
    core.ensure_writable(cluster)?;
    let handle = core.clusters.require(cluster).map_err(|e| e.to_string())?;
    let mut ended = 0;
    let mut result = Ok(());
    for id in silences {
        match alertmanager::delete_silence(&handle, target, id).await {
            Ok(()) => ended += 1,
            Err(err) => {
                result = Err(err);
                break;
            }
        }
    }
    core.record_audit(cluster, "expireSilence", alertname, &result);
    result.map(|()| ended)
}
