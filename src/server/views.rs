use super::*;
use crate::control::recovery::approval::{
    ApprovalRecord, ApprovalState, AssessmentSource, ReviewerConfig,
};

pub(super) fn bootstrap(state: &Console) -> Result<Value, ApiError> {
    let approvals = history::approval_page(
        state,
        &history::ListQuery {
            state: Some("attention".into()),
            ..Default::default()
        },
    )?;
    let repairs = history::repair_page(state, &history::ListQuery::default())?;
    let operations = history::operation_page(state, &history::ListQuery::default())?;
    let simulations = history::simulation_page(state, &history::ListQuery::default())?;
    let extension_statuses = state
        .extensions
        .as_ref()
        .map(|registry| registry.statuses())
        .unwrap_or_default();
    let harnesses=state.registry.as_ref().map(|registry|registry.definitions().iter().map(|h| {
        let node_status=h.address.strip_prefix("node://").and_then(|id|extension_statuses.iter().find(|status|status.id==id));
        json!({
        "id":h.id,"adapter":h.adapter,"address":h.address,"enabled":h.enabled,"isDefault":registry.default_harness()==Some(&h.id),
        "availability":if !h.enabled {"disabled"} else if node_status.is_some_and(|status|!status.available) {"unavailable"} else {"configured"},"authentication":"not_checked","runtimeStatus":"not_probed",
        "reason":node_status.and_then(|status|status.error.as_ref()),
        "workspaceRoots":h.workspace_roots,"projects":null})}).collect::<Vec<_>>()).unwrap_or_default();
    let capability = |id: &str,
                      name: &str,
                      available: bool,
                      actions: Vec<&str>,
                      limitation: &str| {
        json!({"id":id,"name":name,"group":"服务功能",
        "state":match id { "harness" | "text-repair" | "external-plugins" => "limited", "simulation" => "prototype", _ => "implemented" },"availability":if available {"available"} else {"unconfigured"},
        "actions":actions,"summary":name,"limitation":limitation,"availableNow":if available {"已配置"} else {"未配置"}})
    };
    let caps = vec![
        capability(
            "monitoring",
            "监控与故障",
            state.monitoring.is_some(),
            vec!["monitor.read", "incident.read", "incident.acknowledge"],
            "按规则检查异常和采集完整性，保存故障记录；确认收到不解除故障，监控本身不自动执行修复。",
        ),
        capability(
            "harness",
            "AI 会话",
            state.registry.is_some(),
            vec!["harness.run", "harness.projects"],
            "AI 服务提供方是否已登录，以调用结果为准；会话在客户端中的项目分组需另行确认。",
        ),
        capability(
            "text-repair",
            "审批与文本修复",
            state.repair.is_some(),
            vec![
                "repair.run",
                "approval.decide",
                "approval.apply",
                "approval.check_result",
            ],
            "真实动作仅为 Windows 白名单文本替换；读回一致不等于业务恢复。",
        ),
        capability(
            "recovery",
            "自动恢复流程",
            state.recovery.is_some(),
            vec![
                "recovery.read",
                "recovery.decide",
                "recovery.resume",
                "recovery.check_result",
                "knowledge.read",
            ],
            "启用后根据故障启动恢复，统一管理审批、执行许可、验收、未知执行结果和修复经验。",
        ),
        capability(
            "simulation",
            "模拟实验室",
            true,
            vec!["simulation.run"],
            "模拟测试，不操作真实目标。",
        ),
        capability(
            "logs",
            "运行记录",
            true,
            vec!["logs.read"],
            "查询已保存的操作记录和配置允许读取的采集记录；可读内容由采集节点或插件决定。",
        ),
        capability(
            "external-plugins",
            "外部节点与业务插件",
            extension_statuses.iter().any(|status| status.available),
            vec!["extension.read"],
            "只调用已登记且获准的接口与方法；登记插件不授予修改目标的权限。",
        ),
    ];
    let active=state.repair_config.as_ref().map(|c|json!({"name":"当前服务配置","repairConfig":state.config.repair_config,"harnessConfig":c.harness_config,
        "targetId":c.target_id,"targetRoot":c.target_root,"reviewerDirectory":c.reviewer_directory,"dataDirectory":c.data_dir,"allowedFiles":c.allowed_files,
        "executionHarness":c.execution_harness,"executionWorkspace":c.execution_workspace,"reviewerWorkspace":c.reviewer_workspace,
        "reviewer":reviewer(&c.policy.reviewer),"delegation":c.policy.delegation,"policyId":c.policy.id,"policyVersion":c.policy.version,
        "ttlSeconds":c.policy.ttl_secs,"timeoutSeconds":c.timeout_secs,"maxToolCalls":c.max_tool_calls}));
    Ok(
        json!({"schema_version":1,"mode":"live","runtime":{"status":"connected","updatedAt":timestamp(),"operator":state.config.operator,"permissions":state.config.permissions},
        "capabilities":caps,"harnesses":harnesses,"repairs":repairs["items"],"approvals":approvals["items"],"simulation_tasks":simulations["items"],
        "logs":[],"active_configuration":active,"operations":operations["items"],"extension_statuses":extension_statuses,"monitoring":monitoring::bootstrap(state)?,
        "pages":{"repairs":history::metadata(&repairs),"approvals":history::metadata(&approvals),"operations":history::metadata(&operations),"simulation_tasks":history::metadata(&simulations)},"approval_counts":history::approval_counts(state)?}),
    )
}

fn reviewer(value: &ReviewerConfig) -> String {
    match value {
        ReviewerConfig::Human => "人工".into(),
        ReviewerConfig::Harness { harness_id } => format!("AI 审核 · {harness_id}"),
        ReviewerConfig::HumanThenHarness {
            harness_id,
            human_wait_secs,
            ..
        } => format!(
            "人工优先（{human_wait_secs} 秒）/ AI 审核 · {harness_id}（此文本修复流程未启用自动转交）"
        ),
    }
}
pub(super) fn approval(state: &Console, r: &ApprovalRecord) -> Value {
    let operation = &r.request.operation;
    let action = &operation.action;
    let policy = &r.request.policy;
    let execution_context = &action["execution_context"];
    let receipt = r
        .note
        .as_ref()
        .and_then(|note| serde_json::from_str::<Value>(note).ok())
        .map(|raw| {
            json!({
        "contentVerified":raw.get("content_verified").and_then(Value::as_bool)==Some(true),
        "businessVerified":false,"raw":raw})
        });
    let mut actions = Vec::new();
    let unexpired = r.request.expires_at > timestamp() / 1000;
    if state.require("approval.decide").is_ok() {
        if unexpired
            && matches!(
                r.state,
                ApprovalState::Pending | ApprovalState::WaitingHuman
            )
        {
            actions.extend(["approve", "deny"]);
        }
        if matches!(
            r.state,
            ApprovalState::Pending | ApprovalState::WaitingHuman | ApprovalState::Approved
        ) {
            actions.push("revoke");
        }
    }
    if state.require("approval.apply").is_ok() && r.state == ApprovalState::Approved && unexpired {
        actions.push("apply");
    }
    if state.require("approval.check_result").is_ok() && r.state == ApprovalState::Unknown {
        actions.push("check_result");
    }
    let assessment=r.assessment.as_ref().map(|a|{let (source,name,session)=match &a.reviewer {
        AssessmentSource::Human{actor}=>("human",actor.clone(),None),
        AssessmentSource::Harness{harness_id,session_id}=>("harness",harness_id.clone(),Some(session_id.clone())),
    };json!({"decision":a.decision,"reason":a.reason,"source":source,"reviewer":name,"sessionId":session})});
    json!({"requestId":r.request.request_id,"state":r.state,"revision":r.revision,"createdAt":r.request.created_at*1000,"updatedAt":r.updated_at*1000,"expiresAt":r.request.expires_at*1000,
        "taskId":operation.task_id,"taskRevision":operation.task_revision,"operationId":operation.operation_id,"target":operation.target,
        "actionKind":action["kind"],"targetRoot":action["target_root"],"path":action["path"],"expected":action["expected"],"replacement":action["replacement"],
        "userRequest":action["user_request"],"assessment":assessment,"note":r.note,"allowedActions":actions,"normalizedOperation":operation,
        "policy":{"id":policy.id,"version":policy.version,"reviewer":reviewer(&policy.reviewer),"delegation":policy.delegation,
            "allowedTargets":policy.allowed_targets,"allowedActions":policy.allowed_action_kinds,"ttlSeconds":policy.ttl_secs},
        "executionContext":{"harnessId":execution_context["harness_id"],"threadId":execution_context["thread_id"],"turnId":execution_context["turn_id"],"callId":execution_context["call_id"]},
        "receipt":receipt})
}
