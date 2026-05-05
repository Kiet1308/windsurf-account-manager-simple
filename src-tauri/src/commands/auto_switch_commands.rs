use crate::commands::api_commands::{apply_plan_status_to_account, ensure_valid_token_with_force};
use crate::commands::switch_account_commands::switch_account_internal;
use crate::commands::windsurf_info::get_current_windsurf_info;
use crate::models::{Account, AccountStatus, OperationLog, OperationStatus, OperationType};
use crate::repository::DataStore;
use crate::services::{AuthContext, WindsurfService};
use crate::utils::AppError;
use log::{info, warn};
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::{AppHandle, State};
use uuid::Uuid;

fn is_401_result(result: &Value) -> bool {
    result
        .get("status_code")
        .and_then(|v| v.as_u64())
        .map(|code| code == 401)
        .unwrap_or(false)
}

fn is_quota_exhausted(account: &Account, threshold: i32) -> bool {
    account
        .daily_quota_remaining_percent
        .map(|value| value <= threshold)
        .unwrap_or(false)
        || account
            .weekly_quota_remaining_percent
            .map(|value| value <= threshold)
            .unwrap_or(false)
}

fn has_usable_quota(account: &Account, threshold: i32) -> bool {
    let has_quota_data = account.daily_quota_remaining_percent.is_some()
        || account.weekly_quota_remaining_percent.is_some();
    has_quota_data && !is_quota_exhausted(account, threshold)
}

fn is_switch_candidate(account: &Account) -> bool {
    if matches!(&account.status, AccountStatus::Error(_)) {
        return false;
    }
    if account.is_disabled.unwrap_or(false) || account.subscription_active == Some(false) {
        return false;
    }
    if account.is_devin_account() {
        return account.token.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
    }
    account
        .refresh_token
        .as_ref()
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

fn apply_current_user_to_account(user_info: &Value, account: &mut Account) {
    if let Some(user) = user_info.get("user") {
        if let Some(api_key) = user.get("api_key").and_then(|v| v.as_str()) {
            account.windsurf_api_key = Some(api_key.to_string());
        }
        if let Some(disable_codeium) = user.get("disable_codeium").and_then(|v| v.as_bool()) {
            account.is_disabled = Some(disable_codeium);
        }
    }

    if let Some(plan) = user_info.get("plan") {
        if let Some(plan_name) = plan.get("plan_name").and_then(|v| v.as_str()) {
            account.plan_name = Some(plan_name.to_string());
        }
        if let Some(billing_strategy) = plan.get("billing_strategy").and_then(|v| v.as_i64()) {
            account.billing_strategy = Some(billing_strategy as i32);
        }
    }

    if let Some(subscription) = user_info.get("subscription") {
        if let Some(used_quota) = subscription.get("used_quota").and_then(|v| v.as_i64()) {
            account.used_quota = Some(used_quota as i32);
        }
        if let Some(total_quota) = subscription.get("quota").and_then(|v| v.as_i64()) {
            account.total_quota = Some(total_quota as i32);
        }
        if let Some(expires_at) = subscription.get("expires_at").and_then(|v| v.as_i64()) {
            account.subscription_expires_at = chrono::DateTime::from_timestamp(expires_at, 0);
        }
        if let Some(subscription_active) = subscription.get("subscription_active").and_then(|v| v.as_bool()) {
            account.subscription_active = Some(subscription_active);
        }
    }

    account.is_team_owner = Some(
        user_info
            .get("is_root_admin")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    );
    account.last_quota_update = Some(chrono::Utc::now());
}

async fn fetch_current_user_with_retry(
    store: &Arc<DataStore>,
    account: &mut Account,
    service: &WindsurfService,
) -> Result<Value, String> {
    let uuid = account.id;
    let ctx = AuthContext::from_account(account).map_err(|e| e.to_string())?;
    let result = service
        .get_current_user(&ctx)
        .await
        .map_err(|e: AppError| e.to_string())?;

    if is_401_result(&result) {
        ensure_valid_token_with_force(store, account, uuid, true).await?;
        let ctx = AuthContext::from_account(account).map_err(|e| e.to_string())?;
        return service
            .get_current_user(&ctx)
            .await
            .map_err(|e: AppError| e.to_string());
    }

    Ok(result)
}

async fn fetch_plan_status_with_retry(
    store: &Arc<DataStore>,
    account: &mut Account,
    service: &WindsurfService,
) -> Result<Value, String> {
    let uuid = account.id;
    let ctx = AuthContext::from_account(account).map_err(|e| e.to_string())?;
    let result = service
        .get_plan_status(&ctx)
        .await
        .map_err(|e: AppError| e.to_string())?;

    if is_401_result(&result) {
        ensure_valid_token_with_force(store, account, uuid, true).await?;
        let ctx = AuthContext::from_account(account).map_err(|e| e.to_string())?;
        return service
            .get_plan_status(&ctx)
            .await
            .map_err(|e: AppError| e.to_string());
    }

    Ok(result)
}

async fn refresh_account_quota(
    store: &Arc<DataStore>,
    account_id: Uuid,
    use_lightweight_api: bool,
) -> Result<Account, String> {
    let mut account = store.get_account(account_id).await.map_err(|e| e.to_string())?;
    ensure_valid_token_with_force(store, &mut account, account_id, false).await?;

    let service = WindsurfService::new();
    let mut current_user_result: Option<Value> = None;

    if !use_lightweight_api {
        match fetch_current_user_with_retry(store, &mut account, &service).await {
            Ok(result) => {
                current_user_result = Some(result);
            }
            Err(error) => warn!("Auto-switch GetCurrentUser failed for {}: {}", account.email, error),
        }
    }

    let plan_result = fetch_plan_status_with_retry(store, &mut account, &service).await?;
    let mut updated_account = store.get_account(account_id).await.map_err(|e| e.to_string())?;
    if let Some(result) = current_user_result {
        if let Some(user_info) = result.get("user_info") {
            apply_current_user_to_account(user_info, &mut updated_account);
        }
    }
    if plan_result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        if let Some(plan_status) = plan_result.get("plan_status") {
            apply_plan_status_to_account(plan_status, &mut updated_account);
        }
    } else {
        return Err(
            plan_result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("Failed to refresh quota")
                .to_string(),
        );
    }

    store
        .update_account(updated_account.clone())
        .await
        .map_err(|e| e.to_string())?;
    Ok(updated_account)
}

#[tauri::command]
pub async fn check_and_auto_switch_account(
    app: AppHandle,
    store: State<'_, Arc<DataStore>>,
) -> Result<Value, String> {
    let settings = store.get_settings().await.map_err(|e| e.to_string())?;
    if !settings.auto_switch_account_enabled {
        return Ok(json!({
            "success": true,
            "enabled": false,
            "switched": false,
            "reason": "Auto-switch is disabled"
        }));
    }

    let threshold = settings.auto_switch_quota_threshold.clamp(0, 100);
    let current_info = get_current_windsurf_info().map_err(|e| e.to_string())?;
    let current_email = match current_info.email.as_ref().map(|v| v.trim()).filter(|v| !v.is_empty()) {
        Some(email) => email.to_string(),
        None => {
            return Ok(json!({
                "success": true,
                "switched": false,
                "reason": "No active Windsurf account detected"
            }));
        }
    };

    let accounts = store.get_all_accounts().await.map_err(|e| e.to_string())?;
    let current_index = match accounts
        .iter()
        .position(|account| account.email.eq_ignore_ascii_case(&current_email))
    {
        Some(index) => index,
        None => {
            return Ok(json!({
                "success": true,
                "switched": false,
                "current_email": current_email,
                "reason": "The active Windsurf account is not managed by this app"
            }));
        }
    };

    let current_account = refresh_account_quota(
        store.inner(),
        accounts[current_index].id,
        settings.use_lightweight_api,
    )
    .await?;

    if !is_quota_exhausted(&current_account, threshold) {
        return Ok(json!({
            "success": true,
            "switched": false,
            "current_account_id": current_account.id,
            "current_email": current_account.email,
            "daily_quota_remaining_percent": current_account.daily_quota_remaining_percent,
            "weekly_quota_remaining_percent": current_account.weekly_quota_remaining_percent,
            "threshold": threshold,
            "reason": "Current account quota is still above the threshold"
        }));
    }

    for offset in 1..accounts.len() {
        let candidate = &accounts[(current_index + offset) % accounts.len()];
        if candidate.id == current_account.id || !is_switch_candidate(candidate) {
            continue;
        }

        let refreshed_candidate = match refresh_account_quota(
            store.inner(),
            candidate.id,
            settings.use_lightweight_api,
        )
        .await
        {
            Ok(account) => account,
            Err(error) => {
                warn!("Auto-switch skipped {}: {}", candidate.email, error);
                continue;
            }
        };

        if !has_usable_quota(&refreshed_candidate, threshold) {
            continue;
        }

        info!(
            "Auto-switching from {} to {}",
            current_account.email, refreshed_candidate.email
        );
        let switch_result = switch_account_internal(
            &app,
            &refreshed_candidate.id.to_string(),
            store.inner(),
            Some(current_info.client_type.clone()),
        )
        .await?;

        let switched = switch_result
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if switched {
            let log = OperationLog::new(
                OperationType::SwitchAccount,
                OperationStatus::Success,
                format!(
                    "Auto-switch account: {} -> {}",
                    current_account.email, refreshed_candidate.email
                ),
            )
            .with_account(refreshed_candidate.id, refreshed_candidate.email.clone());
            let _ = store.add_log(log).await;
        }

        return Ok(json!({
            "success": switched,
            "switched": switched,
            "current_account_id": current_account.id,
            "current_email": current_account.email,
            "target_account_id": refreshed_candidate.id,
            "target_email": refreshed_candidate.email,
            "daily_quota_remaining_percent": current_account.daily_quota_remaining_percent,
            "weekly_quota_remaining_percent": current_account.weekly_quota_remaining_percent,
            "target_daily_quota_remaining_percent": refreshed_candidate.daily_quota_remaining_percent,
            "target_weekly_quota_remaining_percent": refreshed_candidate.weekly_quota_remaining_percent,
            "threshold": threshold,
            "client_type": current_info.client_type,
            "message": if switched { "Auto-switch completed" } else { "Auto-switch failed" },
            "switch_result": switch_result
        }));
    }

    Ok(json!({
        "success": true,
        "switched": false,
        "current_account_id": current_account.id,
        "current_email": current_account.email,
        "daily_quota_remaining_percent": current_account.daily_quota_remaining_percent,
        "weekly_quota_remaining_percent": current_account.weekly_quota_remaining_percent,
        "threshold": threshold,
        "reason": "No available account with enough daily and weekly quota was found"
    }))
}
