//! Build-time command metadata for generated shell completions and man pages.

use clap::{Arg, ArgAction, Command};

const DATE: &str = "DATE";
const ID: &str = "ID";

#[must_use]
pub(crate) fn command() -> Command {
    Command::new("ccplan")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Agent-authorable cross-platform CLI day planner")
        .subcommand(set_command())
        .subcommand(add_command())
        .subcommand(remind_command())
        .subcommand(edit_command())
        .subcommand(block_target_command("rm"))
        .subcommand(block_target_command("done"))
        .subcommand(block_target_command("skip"))
        .subcommand(snooze_command())
        .subcommand(clear_command())
        .subcommand(read_command("show"))
        .subcommand(read_command("now"))
        .subcommand(read_command("next"))
        .subcommand(read_command("agenda"))
        .subcommand(watch_command())
        .subcommand(serve_command())
        .subcommand(apply_command())
        .subcommand(diff_command())
        .subcommand(approve_command())
        .subcommand(materialize_command())
        .subcommand(fire_command())
        .subcommand(Command::new("roll").hide(true))
        .subcommand(log_command())
        .subcommand(template_command())
        .subcommand(Command::new("status"))
        .subcommand(Command::new("doctor"))
        .subcommand(completions_command())
        .subcommand(Command::new("mcp"))
        .subcommand(gui_command())
}

fn gui_command() -> Command {
    Command::new("gui").about("Open the Cockpit desktop app")
}

fn set_command() -> Command {
    Command::new("set")
        .arg(
            Arg::new("from")
                .long("from")
                .required(true)
                .value_name("PATH|-"),
        )
        .arg(date_arg())
        .arg(flag("override_history", "override-history"))
}

fn add_command() -> Command {
    Command::new("add")
        .arg(date_arg())
        .arg(Arg::new("id").long("id").value_name(ID))
        .arg(Arg::new("title").long("title").required(true))
        .arg(Arg::new("start").long("start").required(true))
        .arg(Arg::new("end").long("end"))
        .arg(Arg::new("duration").long("duration"))
        .arg(Arg::new("notify").long("notify"))
        .arg(Arg::new("tags").long("tags").value_delimiter(','))
        .arg(Arg::new("run").long("run").value_name("ARGV").num_args(1..))
        .arg(Arg::new("every").long("every"))
        .arg(Arg::new("until").long("until").value_name(DATE))
        .arg(Arg::new("count").long("count"))
        .arg(Arg::new("after").long("after").value_delimiter(','))
        .arg(Arg::new("retry").long("retry").value_name("COUNT:BACKOFF"))
        .arg(Arg::new("expect_by").long("expect-by"))
}

fn remind_command() -> Command {
    Command::new("remind")
        .arg(Arg::new("text").required(true))
        .arg(Arg::new("fire_in").long("in").required(true))
        .arg(Arg::new("id").long("id").value_name(ID))
}

fn edit_command() -> Command {
    Command::new("edit")
        .arg(Arg::new("id").required(true).value_name(ID))
        .arg(date_arg())
        .arg(Arg::new("title").long("title"))
        .arg(Arg::new("start").long("start"))
        .arg(Arg::new("end").long("end"))
        .arg(Arg::new("duration").long("duration"))
        .arg(Arg::new("notify").long("notify"))
        .arg(Arg::new("run").long("run").value_name("ARGV").num_args(1..))
}

fn block_target_command(name: &'static str) -> Command {
    Command::new(name).arg(Arg::new("id").required(true).value_name(ID))
}

fn snooze_command() -> Command {
    Command::new("snooze")
        .arg(Arg::new("id").required(true).value_name(ID))
        .arg(Arg::new("by").long("by").required(true))
        .arg(date_arg())
}

fn clear_command() -> Command {
    Command::new("clear")
        .arg(date_arg())
        .arg(flag("yes", "yes"))
        .arg(flag("purge", "purge"))
        .arg(flag("dry_run", "dry-run"))
}

fn read_command(name: &'static str) -> Command {
    Command::new(name).arg(date_arg()).arg(flag("json", "json"))
}

fn watch_command() -> Command {
    Command::new("watch")
        .arg(date_arg())
        .arg(Arg::new("every").long("every").default_value("30s"))
}

fn serve_command() -> Command {
    Command::new("serve")
        .arg(date_arg())
        .arg(Arg::new("agent").long("agent").value_name("NAME"))
        .arg(Arg::new("every").long("every").default_value("30s"))
        .arg(flag("once", "once"))
}

fn apply_command() -> Command {
    Command::new("apply")
        .arg(date_arg())
        .arg(flag("dry_run", "dry-run"))
}

fn diff_command() -> Command {
    Command::new("diff").arg(date_arg())
}

fn approve_command() -> Command {
    Command::new("approve")
        .arg(Arg::new("id").required(true).value_name(ID))
        .arg(date_arg())
}

fn materialize_command() -> Command {
    Command::new("materialize").arg(
        Arg::new("horizon")
            .long("horizon")
            .default_value("14")
            .value_name("N"),
    )
}

fn fire_command() -> Command {
    Command::new("fire")
        .arg(date_arg().required(true))
        .arg(Arg::new("id").long("id").required(true).value_name(ID))
        .arg(
            Arg::new("event")
                .long("event")
                .required(true)
                .value_parser(["notify", "start", "end"]),
        )
        .arg(Arg::new("rev").long("rev").required(true))
        .arg(Arg::new("at").long("at").required(true))
        .arg(Arg::new("attempt").long("attempt").default_value("0"))
        .arg(flag("dry_run", "dry-run"))
}

fn log_command() -> Command {
    Command::new("log")
        .arg(date_arg())
        .arg(Arg::new("since").long("since"))
        .arg(flag("json", "json"))
}

fn template_command() -> Command {
    Command::new("template")
        .subcommand(template_name_command("save"))
        .subcommand(Command::new("list"))
        .subcommand(template_apply_command())
}

fn template_name_command(name: &'static str) -> Command {
    Command::new(name)
        .arg(Arg::new("name").required(true).value_name("NAME"))
        .arg(date_arg())
}

fn template_apply_command() -> Command {
    Command::new("apply")
        .arg(Arg::new("name").required(true).value_name("NAME"))
        .arg(date_arg())
        .arg(
            Arg::new("vars")
                .long("var")
                .value_name("NAME=VALUE")
                .action(ArgAction::Append),
        )
}

fn completions_command() -> Command {
    Command::new("completions").arg(Arg::new("shell").required(true).value_parser([
        "bash",
        "zsh",
        "fish",
        "powershell",
    ]))
}

fn date_arg() -> Arg {
    Arg::new("date").long("date").value_name(DATE)
}

fn flag(id: &'static str, long: &'static str) -> Arg {
    Arg::new(id).long(long).action(ArgAction::SetTrue)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use clap::{Command, CommandFactory};

    use crate::cli::Cli;

    #[test]
    fn build_command_tracks_derive_command_surface() {
        assert_command_surface(&Cli::command(), &super::command());
    }

    fn assert_command_surface(derived: &Command, generated: &Command) {
        assert_eq!(derived.get_name(), generated.get_name());
        assert_eq!(
            collect_subcommand_names(derived),
            collect_subcommand_names(generated),
            "generated command builder must list the same subcommands as the derive parser",
        );
        assert_eq!(
            collect_arg_surface_by_path(derived, ""),
            collect_arg_surface_by_path(generated, ""),
            "generated command builder must list the same arguments as the derive parser",
        );
    }

    fn collect_subcommand_names(command: &Command) -> Vec<String> {
        command
            .get_subcommands()
            .map(|subcommand| subcommand.get_name().to_owned())
            .collect()
    }

    fn collect_arg_surface_by_path(command: &Command, path: &str) -> BTreeMap<String, Vec<String>> {
        let current_path = if path.is_empty() {
            command.get_name().to_owned()
        } else {
            format!("{path} {}", command.get_name())
        };
        let mut args = command
            .get_arguments()
            .map(|arg| {
                format!(
                    "{}|long={:?}|index={:?}|required={}",
                    arg.get_id().as_str(),
                    arg.get_long(),
                    arg.get_index(),
                    arg.is_required_set(),
                )
            })
            .collect::<Vec<_>>();
        args.sort();
        let mut result = BTreeMap::from([(current_path.clone(), args)]);
        for subcommand in command.get_subcommands() {
            result.extend(collect_arg_surface_by_path(subcommand, &current_path));
        }
        result
    }
}
