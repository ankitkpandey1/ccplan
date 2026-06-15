use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use clap_complete::{
    Generator, generate_to,
    shells::{Bash, Fish, PowerShell, Zsh},
};

#[path = "src/cli_command.rs"]
mod cli_command;

const BIN_NAME: &str = "ccplan";

fn main() -> Result<(), Box<dyn Error>> {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);

    let bash = generate_completion(Bash, &out_dir)?;
    let zsh = generate_completion(Zsh, &out_dir)?;
    let fish = generate_completion(Fish, &out_dir)?;
    let powershell = generate_completion(PowerShell, &out_dir)?;
    let manpage = generate_manpage(&out_dir)?;

    rustc_path_env("CCPLAN_COMPLETION_BASH", &bash);
    rustc_path_env("CCPLAN_COMPLETION_ZSH", &zsh);
    rustc_path_env("CCPLAN_COMPLETION_FISH", &fish);
    rustc_path_env("CCPLAN_COMPLETION_POWERSHELL", &powershell);
    rustc_path_env("CCPLAN_MANPAGE", &manpage);

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/cli_command.rs");
    Ok(())
}

fn generate_completion<G>(generator: G, out_dir: &Path) -> Result<PathBuf, Box<dyn Error>>
where
    G: Generator,
{
    let mut command = cli_command::command();
    Ok(generate_to(generator, &mut command, BIN_NAME, out_dir)?)
}

fn generate_manpage(out_dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let path = out_dir.join("ccplan.1");
    let mut buffer = Vec::new();
    clap_mangen::Man::new(cli_command::command()).render(&mut buffer)?;
    fs::write(&path, buffer)?;
    Ok(path)
}

fn rustc_path_env(key: &str, path: &Path) {
    println!("cargo:rustc-env={key}={}", path.display());
}
