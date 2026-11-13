mod app;
mod apps;
mod brew;
mod matcher;
mod scan;
mod size;
mod spinner;
mod trash;
mod ui;
mod update;

use app::App;
use ratatui::crossterm::event::{self, Event};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use std::io::IsTerminal;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        None => {}
        Some("--version") => {
            println!("cleaner {}", update::CURRENT);
            return Ok(());
        }
        Some("update") => {
            if let Err(message) = update_command(&args[2..]) {
                eprintln!("cleaner: {message}");
                std::process::exit(1);
            }
            return Ok(());
        }
        Some(_) => {
            eprintln!("Usage: cleaner [update | --version]");
            std::process::exit(2);
        }
    }
    let (tx, rx) = mpsc::channel();
    let mut app = App::new(tx);
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, &rx);
    ratatui::restore();
    result
}

fn update_command(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err("Usage: cleaner update.".into());
    }
    if !update::enabled() {
        return Err("Only release builds update themselves. Rebuild from source instead.".into());
    }
    // `off` only stops the automatic check, so an explicit update with it follows stable releases.
    let progress = spinner::Spinner::start();
    let download_started = std::cell::OnceCell::new();
    let installed = update::install_now(update::channel(), &|step| match step {
        update::Progress::Checking => progress.set("Check for updates", String::new(), String::new()),
        update::Progress::Downloading { version, received, total } => {
            let elapsed = download_started.get_or_init(std::time::Instant::now).elapsed();
            progress.set(&format!("Download v{version}"), spinner::transfer(received, total, elapsed), spinner::mib(received));
        }
        update::Progress::Verifying => progress.set("Verify the download", String::new(), String::new()),
        update::Progress::Installing => progress.set("Replace the binary", String::new(), String::new()),
    });
    progress.finish(installed.is_ok());
    let styled = std::io::stdout().is_terminal();
    match installed? {
        Some(version) => println!(
            "\n{} {} → {}  {}",
            paint(styled, spinner::BOLD, "cleaner"),
            paint(styled, spinner::DIM, &format!("v{}", update::CURRENT)),
            paint(styled, &format!("{}{}", spinner::BOLD, spinner::GREEN), &format!("v{version}")),
            paint(styled, spinner::DIM, &format!("{}/tag/v{version}", update::RELEASES)),
        ),
        None => println!("\n{} {} is up to date.", paint(styled, spinner::BOLD, "cleaner"), paint(styled, spinner::GREEN, &format!("v{}", update::CURRENT))),
    }
    Ok(())
}

fn paint(styled: bool, style: &str, text: &str) -> String {
    if styled { format!("{style}{text}{}", spinner::RESET) } else { text.to_string() }
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App, rx: &mpsc::Receiver<app::Msg>) -> std::io::Result<()> {
    while !app.quit {
        terminal.draw(|frame| ui::draw(frame, app))?;
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            app.key(key);
        }
        while let Ok(msg) = rx.try_recv() {
            app.handle(msg);
        }
        if let Some(batch) = app.admin.take() {
            ratatui::restore();
            run_as_admin(&batch.pending);
            reenter(terminal)?;
            app.settle(batch);
        }
        if let Some(commands) = app.brew_run.take() {
            ratatui::restore();
            let succeeded = run_brew(&commands);
            reenter(terminal)?;
            app.brew_finished(&commands, succeeded);
        }
    }
    Ok(())
}

fn reenter(terminal: &mut ratatui::DefaultTerminal) -> std::io::Result<()> {
    // ratatui::init would install one more panic hook per prompt.
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    terminal.clear()
}

/// Confirmed brew commands run in the plain terminal, so brew's own output and prompts show.
fn run_brew(commands: &[brew::Args]) -> bool {
    let Some(program) = brew::program() else { return false };
    // Ctrl-C reaches the whole process group: only brew should stop.
    // SAFETY: changes this process's SIGINT disposition, restored below.
    unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
    let mut succeeded = true;
    for args in commands {
        println!("\n$ brew {}", args.join(" "));
        if !succeeds(Command::new(&program).args(args)) {
            succeeded = false;
            break;
        }
    }
    print!("\n{} Press Enter to go back to cleaner. ", if succeeded { "Done." } else { "brew failed." });
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let _ = std::io::stdin().read_line(&mut String::new());
    // SAFETY: back to the default disposition the process started with.
    unsafe { libc::signal(libc::SIGINT, libc::SIG_DFL) };
    succeeded
}

/// Root-owned files move with sudo in the plain terminal, after one password prompt.
fn run_as_admin(moves: &[trash::Move]) {
    println!("Cleaner needs administrator rights to move {}:", trash::plural(moves.len(), "item"));
    for moved in moves {
        println!("  {}\n    to {}", moved.from.display(), moved.to.display());
    }
    // Ctrl-C at the prompt reaches the whole process group: only sudo should stop.
    // SAFETY: changes this process's SIGINT disposition, restored below.
    unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
    if succeeds(Command::new("sudo").args(["-p", "Password for %u (Ctrl-C skips these items): ", "-v"])) {
        for moved in moves {
            if let Some(parent) = moved.to.parent() {
                succeeds(Command::new("sudo").args(["/bin/mkdir", "-p", "--"]).arg(parent));
            }
            succeeds(Command::new("sudo").args(["/bin/mv", "-n", "--"]).arg(&moved.from).arg(&moved.to));
        }
    }
    // SAFETY: back to the default disposition the process started with.
    unsafe { libc::signal(libc::SIGINT, libc::SIG_DFL) };
}

fn succeeds(command: &mut Command) -> bool {
    use std::os::unix::process::CommandExt;
    // SAFETY: only async-signal-safe signal() runs between fork and exec.
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            Ok(())
        })
    };
    command.status().is_ok_and(|status| status.success())
}
