use anyhow::{Context, Result};

pub async fn run(
    aws_config: &aws_config::SdkConfig,
    resource: &str,
    name: Option<&str>,
    cluster: &str,
) -> Result<()> {
    if resource != "oabservice" {
        anyhow::bail!("unknown resource type: {}. Use 'oabservice'", resource);
    }

    let ecs = aws_sdk_ecs::Client::new(aws_config);

    let services = if let Some(name) = name {
        // Describe a specific service
        let svc_name = if name.starts_with("oab-") {
            name.to_string()
        } else {
            // Try to find by listing all oab- services and matching the name suffix
            format!("oab-prod-{}", name) // TODO: support --namespace flag
        };
        vec![svc_name]
    } else {
        // List all oab- services
        let mut service_arns = Vec::new();
        let mut next_token = None;
        loop {
            let mut req = ecs.list_services().cluster(cluster);
            if let Some(token) = &next_token {
                req = req.next_token(token);
            }
            let resp = req.send().await.context("failed to list ECS services")?;
            for arn in resp.service_arns() {
                if arn.contains("/oab-") {
                    service_arns.push(arn.to_string());
                }
            }
            next_token = resp.next_token().map(|s| s.to_string());
            if next_token.is_none() {
                break;
            }
        }
        service_arns
    };

    if services.is_empty() {
        println!("No OAB services found.");
        return Ok(());
    }

    // Describe in batches of 10
    println!(
        "{:<12} {:<10} {:<5} {:<6} {:<14} {:<6} STATUS",
        "NAME", "NAMESPACE", "CPU", "MEM", "CAPACITY", "TASKS"
    );

    for chunk in services.chunks(10) {
        let resp = ecs
            .describe_services()
            .cluster(cluster)
            .set_services(Some(chunk.to_vec()))
            .send()
            .await
            .context("failed to describe ECS services")?;

        for svc in resp.services() {
            let svc_name = svc.service_name().unwrap_or("-");
            // Parse oab-{namespace}-{name}
            let parts: Vec<&str> = svc_name.splitn(3, '-').collect();
            let (namespace, agent_name) = if parts.len() == 3 {
                (parts[1], parts[2])
            } else {
                ("?", svc_name)
            };

            let status = svc.status().unwrap_or("UNKNOWN");
            let running = svc.running_count();
            let desired = svc.desired_count();

            // Get cpu/memory from task definition
            let (cpu, mem) = if let Some(td_arn) = svc.task_definition() {
                let td_resp = ecs
                    .describe_task_definition()
                    .task_definition(td_arn)
                    .send()
                    .await;
                if let Ok(td) = td_resp {
                    let td = td.task_definition();
                    let cpu = td.and_then(|t| t.cpu()).unwrap_or("-");
                    let mem = td.and_then(|t| t.memory()).unwrap_or("-");
                    (cpu.to_string(), mem.to_string())
                } else {
                    ("-".to_string(), "-".to_string())
                }
            } else {
                ("-".to_string(), "-".to_string())
            };

            let cap = svc
                .capacity_provider_strategy()
                .first()
                .map(|c| c.capacity_provider())
                .unwrap_or("FARGATE");

            println!(
                "{:<12} {:<10} {:<5} {:<6} {:<14} {}/{:<3} {}",
                agent_name, namespace, cpu, mem, cap, running, desired, status
            );
        }
    }

    Ok(())
}
