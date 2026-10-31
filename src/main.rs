use clap::Parser;

fn main() {
    let cli = anyarc::cli::Cli::parse();
    let code = match cli.cmd {
        anyarc::cli::Cmd::Extract(a) => run(anyarc::cli::cmd_extract(&a)),
        anyarc::cli::Cmd::Test(a) => run(anyarc::cli::cmd_test(&a)),
        anyarc::cli::Cmd::List(a) => run(anyarc::cli::cmd_list(&a)),
        anyarc::cli::Cmd::Create(a) => run(anyarc::cli::cmd_create(&a)),
        anyarc::cli::Cmd::Formats => run(anyarc::cli::cmd_formats()),
    };
    std::process::exit(code);
}

fn run(r: anyhow::Result<u8>) -> i32 {
    match r {
        Ok(code) => code as i32,
        Err(e) => {
            eprintln!("错误: {e:#}");
            1
        }
    }
}
