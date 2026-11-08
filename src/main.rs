mod app;
mod apps;
mod matcher;
mod scan;
mod size;
mod trash;
mod ui;

use app::App;
use ratatui::crossterm::event::{self, Event};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

fn main() -> std::io::Result<()> {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("--version") => {
            println!("cleaner {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(_) => {
            eprintln!("Usage: cleaner [--version]");
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
            // ratatui::init would install one more panic hook per prompt.
            enable_raw_mode()?;
            execute!(std::io::stdout(), EnterAlternateScreen)?;
            terminal.clear()?;
            app.settle(batch);
        }
    }
    Ok(())
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
