use clap::{Parser, Subcommand};
use std::fs;
use burgertocow_lib::engine::Tracker;
use burgertocow_lib::diff::generate_diff;
use anyhow::Result;

#[derive(Parser)]
#[command(name = "burgertocow")]
#[command(about = "Reverse minijinja template diff generator")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Render {
        #[arg(long)]
        template: String,
        #[arg(long)]
        data: String,
        #[arg(long)]
        out: String,
    },
    Diff {
        #[arg(long)]
        template: String,
        #[arg(long)]
        data: String,
        #[arg(long)]
        modified: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    
    match &cli.command {
        Commands::Render { template, data, out } => {
            let t_str = fs::read_to_string(template)?;
            let d_str = fs::read_to_string(data)?;
            let ctx: serde_json::Value = serde_json::from_str(&d_str)?;
            
            let mut tracker = Tracker::new();
            let (pure, _tracked) = tracker.render(template, &t_str, &ctx)?;
            
            fs::write(out, pure)?;
            println!("Rendered output written to {}", out);
        }
        Commands::Diff { template, data, modified } => {
            let t_str = fs::read_to_string(template)?;
            let d_str = fs::read_to_string(data)?;
            let m_str = fs::read_to_string(modified)?;
            let ctx: serde_json::Value = serde_json::from_str(&d_str)?;
            
            let mut tracker = Tracker::new();
            let (_pure, tracked) = tracker.render(template, &t_str, &ctx)?;
            
            let diff_str = generate_diff(&t_str, &tracked, &m_str);
            print!("{}", diff_str);
        }
    }
    
    Ok(())
}
