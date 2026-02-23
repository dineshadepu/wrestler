use std::collections::HashMap;

use crate::config::{Phase, Problem, Target};

#[derive(Debug)]
pub struct ConcretePhase {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: HashMap<String, String>,
}

#[derive(Debug)]
pub struct RunPlan {
    pub name: String,
    pub run_root: String,
    pub build: Option<ConcretePhase>,
    pub run: ConcretePhase,
    pub analyze: Option<ConcretePhase>,
}

fn replace_tokens(input: &str, map: &HashMap<String, String>) -> String {
    let mut result = input.to_string();
    for (k, v) in map {
        let token = format!("{{{}}}", k);
        result = result.replace(&token, v);
    }
    result
}
fn value_to_string(v: &toml::Value) -> String {
    match v {
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::String(s) => s.clone(),
        _ => v.to_string(),
    }
}

fn build_concrete_phase(
    phase: &Phase,
    substitutions: &HashMap<String, String>,
    default_cwd: &str,
) -> ConcretePhase {
    let program = replace_tokens(&phase.program, substitutions);

    let args = phase
        .args
        .iter()
        .map(|a| replace_tokens(a, substitutions))
        .collect();

    let cwd = match &phase.cwd {
        Some(c) => replace_tokens(c, substitutions),
        None => default_cwd.to_string(),
    };

    let env = phase.env.clone().unwrap_or_default();

    ConcretePhase {
        program,
        args,
        cwd,
        env,
    }
}

use itertools::Itertools;

pub fn plan_problem(
    problem_name: &str,
    problem: &Problem,
    target_name: &str,
    target: &Target,
) -> Vec<RunPlan> {
    let project_root = &target.root;

    // Parameter keys and values
    let keys: Vec<String> = problem.parameters.keys().cloned().collect();
    let values: Vec<Vec<String>> = problem
        .parameters
        .values()
        .map(|vals| vals.iter().map(value_to_string).collect())
        .collect();

    let mut plans = Vec::new();

    for combo in values.into_iter().multi_cartesian_product() {
        // Build substitution map
        let mut subs = HashMap::new();

        for (k, v) in keys.iter().zip(combo.iter()) {
            subs.insert(k.clone(), v.clone());
        }

        // Add project_root
        subs.insert("project_root".into(), project_root.clone());

        // Run name
        let run_name = replace_tokens(&problem.run_name, &subs);

        // Compute run_root
        let run_root = format!(
            "{}/wrestler_outputs/problems/{}/runs/{}/{}",
            project_root, problem_name, target_name, run_name
        );

        subs.insert("run_dir".into(), run_root.clone());

        // Default cwd
        let default_run_cwd = format!("{}/run", run_root);

        let build = problem
            .phases
            .build
            .as_ref()
            .map(|b| build_concrete_phase(b, &subs, project_root));

        let run = build_concrete_phase(&problem.phases.run, &subs, &default_run_cwd);

        let analyze = problem.phases.analyze.as_ref().map(|a| {
            let analysis_dir = format!("{}/analysis", run_root);
            build_concrete_phase(a, &subs, &analysis_dir)
        });

        plans.push(RunPlan {
            name: run_name,
            run_root,
            build,
            run,
            analyze,
        });
    }

    plans
}
