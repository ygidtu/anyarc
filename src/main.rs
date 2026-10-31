use clap::Parser;

fn main() {
    let cli = anyarc::cli::Cli::parse();
    let code = match cli.cmd {
        anyarc::cli::Cmd::Extract(a) => run(anyarc::cli::cmd_extract(&a)),
        anyarc::cli::Cmd::Test(a) => run(anyarc::cli::cmd_test(&a)),
        anyarc::cli::Cmd::List(a) => run(anyarc::cli::cmd_list(&a)),
        anyarc::cli::Cmd::Create(a) => run(anyarc::cli::cmd_create(&a)),
        anyarc::cli::Cmd::Diff(a) => run(anyarc::cli::cmd_diff(&a)),
        anyarc::cli::Cmd::Search(a) => run(anyarc::cli::cmd_search(&a)),
        anyarc::cli::Cmd::Repack(a) => run(anyarc::cli::cmd_repack(&a)),
        anyarc::cli::Cmd::Formats => run(anyarc::cli::cmd_formats()),
        anyarc::cli::Cmd::Completions(a) => run(anyarc::cli::write_completions(a.shell).map(|()| 0)),
        anyarc::cli::Cmd::Man => run(anyarc::cli::write_man().map(|()| 0)),
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
