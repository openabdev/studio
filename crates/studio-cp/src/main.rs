//! Studio control-plane CLI (read-only prototype).
//!
//! Surfaces the Deployment read-model. `list` / `get <service>` are the CLI
//! front-end of ADR-2's read tools (`deploy_list` / `deploy_get`); they only
//! observe (no writes — writes wait on ADR-3 authz). Cluster from `$OAB_CLUSTER`
//! (default `oab`); AWS creds from the default chain.

use studio_cp::{observe_deployment, observe_services};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cluster = std::env::var("OAB_CLUSTER").unwrap_or_else(|_| "oab".into());
    let aws = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;

    match args.get(1).map(String::as_str) {
        Some("list") => {
            let services = observe_services(&aws, &cluster).await?;
            println!("{:<14} {:<10} {:<9} STATUS", "NAME", "NAMESPACE", "TASKS");
            for s in services {
                println!(
                    "{:<14} {:<10} {:>3}/{:<5} {}",
                    s.name, s.namespace, s.running, s.desired, s.status
                );
            }
        }
        Some("get") => {
            let name = args
                .get(2)
                .ok_or_else(|| anyhow::anyhow!("usage: studio-cp get <service>"))?;
            match observe_deployment(&aws, &cluster, name).await? {
                None => println!("no such Deployment: {name}"),
                Some(d) => {
                    println!(
                        "Deployment {}/{}  desired={} current={} ready={}",
                        d.namespace, d.name, d.desired, d.current, d.ready
                    );
                    for i in &d.instances {
                        println!("  {:<10} {}", format!("{:?}", i.phase), i.id);
                    }
                }
            }
        }
        _ => {
            eprintln!("usage: studio-cp <list | get <service>>");
            std::process::exit(2);
        }
    }
    Ok(())
}
