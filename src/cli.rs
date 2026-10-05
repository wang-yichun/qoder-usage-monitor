use std::sync::OnceLock;

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub demo: bool,
    pub preview: bool,
    pub diagnose: bool,
    pub token: Option<String>,
    pub region: Option<String>,
}

static OPTIONS: OnceLock<Options> = OnceLock::new();

pub fn options() -> &'static Options {
    OPTIONS.get_or_init(Options::default)
}

pub fn parse(args: &[String]) -> Options {
    let mut options = Options::default();
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--demo" => options.demo = true,
            "--preview" => options.preview = true,
            "--diagnose" => options.diagnose = true,
            "--token" => {
                index += 1;
                options.token = args.get(index).cloned().filter(|value| !value.is_empty());
            }
            "--region" => {
                index += 1;
                options.region = args
                    .get(index)
                    .cloned()
                    .map(|value| value.to_lowercase())
                    .filter(|value| value == "global" || value == "cn");
            }
            _ => {}
        }
        index += 1;
    }
    if options.token.is_none() {
        options.token = std::env::var("QODER_USAGE_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
    }
    options
}

pub fn init(args: &[String]) {
    let _ = OPTIONS.set(parse(args));
}
