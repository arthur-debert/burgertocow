use minijinja::{Environment, escape_formatter, value::Value, Error};

pub const VAR_START: &str = "\x1E";
pub const VAR_END: &str = "\x1F";

fn tracking_formatter(
    out: &mut minijinja::Output<'_, '_>,
    state: &minijinja::State<'_, '_>,
    value: &Value,
) -> Result<(), Error> {
    out.write_str(VAR_START)?;
    escape_formatter(out, state, value)?;
    out.write_str(VAR_END)?;
    Ok(())
}

pub struct Tracker {
    env: Environment<'static>,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Tracker {
    pub fn new() -> Self {
        let mut env = Environment::new();
        env.set_formatter(tracking_formatter);
        Self { env }
    }

    pub fn render(
        &mut self,
        template_name: &str,
        template_src: &str,
        ctx: &serde_json::Value,
    ) -> Result<(String, String), Error> {
        self.env
            .add_template_owned(template_name.to_string(), template_src.to_string())?;
        let tmpl = self.env.get_template(template_name)?;
        let tracked = tmpl.render(ctx)?;

        // The pure render removes the unprintable markers
        let mut pure = String::with_capacity(tracked.len());
        for c in tracked.chars() {
            if c != '\x1E' && c != '\x1F' {
                pure.push(c);
            }
        }

        Ok((pure, tracked))
    }
}
