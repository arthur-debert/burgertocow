use anyhow::{Context, Result};
use burgertocow::{generate_diff, Tracker};
use clap::{Parser, Subcommand};
use std::fs;

#[derive(Parser)]
#[command(
    name = "burgertocow",
    version,
    about = "Reverse minijinja template diff generator"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Render a template with the given JSON context.
    Render {
        #[arg(long)]
        template: String,
        #[arg(long)]
        data: String,
        #[arg(long)]
        out: String,
    },
    /// Reverse-diff a modified render back against its source template.
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
        Commands::Render {
            template,
            data,
            out,
        } => {
            let t_str = fs::read_to_string(template)
                .with_context(|| format!("reading template {template}"))?;
            let d_str = fs::read_to_string(data).with_context(|| format!("reading data {data}"))?;
            let ctx: serde_json::Value = serde_json::from_str(&d_str)
                .with_context(|| format!("parsing JSON data file {data}"))?;

            let mut tracker = Tracker::new();
            tracker.add_template(template, &t_str)?;
            let tracked = tracker.render(template, &ctx)?;

            fs::write(out, tracked.output()).with_context(|| format!("writing output {out}"))?;
            println!("Rendered output written to {out}");
        }
        Commands::Diff {
            template,
            data,
            modified,
        } => {
            let t_str = fs::read_to_string(template)
                .with_context(|| format!("reading template {template}"))?;
            let d_str = fs::read_to_string(data).with_context(|| format!("reading data {data}"))?;
            let m_str = fs::read_to_string(modified)
                .with_context(|| format!("reading modified {modified}"))?;
            let ctx: serde_json::Value = serde_json::from_str(&d_str)
                .with_context(|| format!("parsing JSON data file {data}"))?;

            let mut tracker = Tracker::new();
            tracker.add_template(template, &t_str)?;
            let tracked = tracker.render(template, &ctx)?;

            let diff_str = generate_diff(&t_str, &tracked, &m_str);
            print!("{diff_str}");
        }
    }

    Ok(())
}
