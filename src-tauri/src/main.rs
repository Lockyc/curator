#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("validate") => std::process::exit(curator_lib::validate_cli(
            args.get(2).map(std::path::PathBuf::from),
        )),
        Some(other) => {
            eprintln!("unknown command: {other}\nusage: curator [validate [path]]");
            std::process::exit(2);
        }
        None => curator_lib::run(),
    }
}
