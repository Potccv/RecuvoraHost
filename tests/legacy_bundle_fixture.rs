use super::*;
pub(super) const TASK: &str = "old-task-random-afe739";
pub(super) const OPERATION: &str = "old-operation-random-c619";
pub(super) const APPROVAL: &str = "approval-0000000000000001";
pub(super) const INCIDENT: &str = "incident-0000000000000001-0000";

pub(super) fn configuration() -> LegacyRecoveryBundleConfig {
    let policy = ApprovalPolicy {
        id: "human-policy".into(),
        version: 1,
        reviewer: ReviewerConfig::Human,
        delegation: "repair the scoped target".into(),
        allowed_targets: vec!["target-a".into()],
        allowed_action_kinds: vec!["execute_script".into()],
        ttl_secs: 60,
    };
    let mut script_policy = policy.clone();
    script_policy.id = "script-policy".into();
    script_policy.reviewer = ReviewerConfig::Harness {
        harness_id: "reviewer".into(),
    };
    LegacyRecoveryBundleConfig {
        recovery: RecoveryConfig {
            schema_version: 1,
            execution_harness: "executor".into(),
            target: TargetBinding {
                target_id: "target-a".into(),
                executor_id: "node-a".into(),
                platform: "portable".into(),
                allowed_languages: vec!["python".into()],
                diagnostic_queries: vec!["snapshot".into()],
                verification_profile: "readiness".into(),
                required_facts: BTreeMap::from([("workload_version".into(), "1".into())]),
                action_timeout_secs: 10,
            },
            approval: policy,
            script_approval: script_policy,
            diagnosis_timeout_secs: 10,
            review_timeout_secs: 10,
            max_tool_calls: 4,
            max_diagnoses: 2,
            minimum_script_occurrences: 1,
            max_tasks: 20,
        },
        recovery_journal_bytes: 1024 * 1024,
        approval: ApprovalStoreConfig::default(),
        knowledge: KnowledgeStoreConfig::default(),
        incidents: IncidentStoreConfig::default(),
    }
}

pub(super) fn write_lines(path: &Path, values: &[Value]) {
    let text = values
        .iter()
        .map(|value| format!("{value}\n"))
        .collect::<String>();
    std::fs::write(path, text).unwrap();
}

pub(super) struct Fixture {
    pub(super) dir: TestDir,
    pub(super) root: PathBuf,
    pub(super) incidents: PathBuf,
    pub(super) config: LegacyRecoveryBundleConfig,
    pub(super) revisions: Vec<Value>,
}
impl Fixture {
    pub(super) fn new(stage: &str, format: u32) -> Self {
        let dir = TestDir::new("legacy-bundle");
        let root = dir.path.join("recovery");
        std::fs::create_dir_all(root.join("approvals")).unwrap();
        std::fs::write(root.join("recovery.lock"), b"").unwrap();
        std::fs::write(root.join("approvals/approvals.lock"), b"").unwrap();
        let config = configuration();
        let mut old_config = serde_json::to_value(&config.recovery).unwrap();
        old_config["max_journal_bytes"] = json!(config.recovery_journal_bytes);
        let script = json!({"id":"script-a","version":1,"language":"python","platform":"portable","source":"bounded legacy fixture","preconditions":{"workload_version":"1"},"generated_by_harness":"executor","generated_in_session":"original-diagnosis-session"});
        let plan = json!({"summary":"bounded repair","script":script,"reusable":true});
        let problem = json!({"incident_id":INCIDENT,"incident_revision":1,"target_id":"target-a","fingerprint":"not-ready","summary":"provider reports unavailable","occurrences":1,"keywords":["readiness"],"conditions":{"workload_version":"1"},"evidence_refs":["provider:original"]});
        let observation = json!({"target_id":"target-a","facts":{"workload_version":"1"},"evidence_refs":["provider:inspection"],"observed_at_ms":1200});
        let operation = json!({"task_id":TASK,"task_revision":3,"operation_id":OPERATION,"target":"target-a","action":{"kind":"execute_script","executor_id":"node-a","script":script,"verification_profile":"readiness","required_facts":{"workload_version":"1"},"timeout_secs":10,"incident_id":INCIDENT,"incident_revision":1}});
        let mut task = json!({"id":TASK,"revision":1,"problem":problem,"episode_count":1,"stage":"queued","diagnosis_attempts":0,"plan":null,"knowledge_id":null,"reused_script":false,"approval_id":null,"operation":null,"observation":null,"receipt":null,"verification":null,"result_check":null,"note":null,"created_at_ms":1000,"updated_at_ms":1000});
        if format == 1 {
            task.as_object_mut().unwrap().remove("episode_count");
        }
        let mut revisions = Vec::new();
        let mut save = |task: &Value| {
            revisions.push(json!({"format":format,"sequence":task["revision"],"config":old_config,"task":task}))
        };
        save(&task);
        task["revision"] = json!(2);
        task["stage"] = json!("diagnosing");
        task["updated_at_ms"] = json!(1100);
        save(&task);
        task["revision"] = json!(3);
        task["diagnosis_attempts"] = json!(1);
        task["observation"] = observation;
        task["updated_at_ms"] = json!(1200);
        save(&task);
        task["revision"] = json!(4);
        task["stage"] = json!("awaiting_approval");
        task["plan"] = plan.clone();
        task["operation"] = operation.clone();
        task["updated_at_ms"] = json!(1300);
        save(&task);
        task["revision"] = json!(5);
        task["approval_id"] = json!(APPROVAL);
        task["updated_at_ms"] = json!(1400);
        save(&task);
        let mut approvals = vec![
            json!({"format":1,"sequence":1,"now":1,"event":{"event":"requested","request":{"request_id":APPROVAL,"operation":operation,"policy":config.recovery.approval,"created_at":1,"expires_at":61}}}),
        ];
        let mut knowledge = Vec::new();
        if stage != "pending" {
            approvals.push(json!({"format":1,"sequence":2,"now":2,"event":{"event":"changed","request_id":APPROVAL,"change":{"change":"human_decision","assessment":{"decision":"approve","reason":"reviewed original operation","reviewer":{"source":"human","actor":"operator"}}}}}));
            task["revision"] = json!(6);
            task["stage"] = json!("executing");
            task["updated_at_ms"] = json!(3000);
            save(&task);
            approvals.push(json!({"format":1,"sequence":3,"now":3,"event":{"event":"changed","request_id":APPROVAL,"change":{"change":"consume"}}}));
        }
        if stage == "completed" {
            approvals.push(json!({"format":1,"sequence":4,"now":4,"event":{"event":"changed","request_id":APPROVAL,"change":{"change":"complete","outcome":"executed","reason":"original executor receipt"}}}));
            task["revision"] = json!(7);
            task["stage"] = json!("verifying");
            task["receipt"] = json!({"operation_id":OPERATION,"target_id":"target-a","outcome":"executed","executor_stopped":true,"evidence_refs":["executor:original"],"summary":"original executor receipt"});
            task["updated_at_ms"] = json!(4000);
            save(&task);
            task["revision"] = json!(8);
            task["stage"] = json!("publishing");
            task["verification"] = json!({"operation_id":OPERATION,"target_id":"target-a","profile":"readiness","healthy":true,"executor_stopped":true,"evidence_refs":["business:original"],"verified_at_ms":5000});
            task["updated_at_ms"] = json!(5000);
            save(&task);
            let candidate_id = format!("case-{TASK}-1");
            let candidate = json!({"id":candidate_id,"incident_id":INCIDENT,"summary":"bounded repair","keywords":["readiness"],"conditions":{"workload_version":"1","platform":"portable","fault_fingerprint":"not-ready"},"script":script,"reusable":true,"evidence_refs":["provider:original"],"created_at_ms":1000});
            knowledge.push(
                json!({"format":1,"sequence":1,"event":{"type":"candidate","candidate":candidate}}),
            );
            knowledge.push(json!({"format":1,"sequence":2,"event":{"type":"outcome","record_id":candidate_id,"case":{"id":format!("{OPERATION}-Verified"),"operation_id":OPERATION,"target_id":"target-a","script_id":"script-a","script_version":1,"outcome":"verified","evidence_refs":["business:original"],"recorded_at_ms":5000},"verification":{"operation_id":OPERATION,"target_id":"target-a","script_id":"script-a","script_version":1,"verifier_id":"node-a:readiness","evidence_refs":["business:original"],"verified_at_ms":5000}}}));
            task["revision"] = json!(9);
            task["stage"] = json!("completed");
            task["knowledge_id"] = json!(candidate_id);
            task["updated_at_ms"] = json!(6000);
            save(&task);
        }
        write_lines(&root.join("recovery.jsonl"), &revisions);
        write_lines(&root.join("approvals/approvals.jsonl"), &approvals);
        write_lines(&root.join("knowledge.jsonl"), &knowledge);
        let incidents = dir.path.join("incidents.jsonl");
        write_lines(
            &incidents,
            &[
                json!({"format":1,"sequence":1,"event":{"type":"monitor","commit":{"monitor_id":"monitor-a","sequence":1,"checkpoint":{"cursor":"original"},"signals":[{"monitor_id":"monitor-a","target_id":"target-a","rule_id":"rule-a","kind":"target","condition":"active","summary":"provider reports unavailable","evidence":{"source":"original"}}],"now_ms":1000}}}),
            ],
        );
        Self {
            dir,
            root,
            incidents,
            config,
            revisions,
        }
    }
    pub(super) fn authority(&self) -> FileTargetOwnership {
        FileTargetOwnership::open(self.dir.path.join("ownership")).unwrap()
    }
    pub(super) fn import(
        &self,
        authority: &dyn TargetOwnership,
    ) -> Result<LegacyRecoveryBundleReport, ImportError> {
        import_legacy_recovery_bundle(
            &self.root,
            &self.incidents,
            self.config.clone(),
            authority,
            10_000,
        )
    }
}
