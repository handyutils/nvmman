use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{self, Stdout},
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, MouseButton,
        MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Row, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table, Wrap,
    },
};
use semver::Version;
use serde::{Deserialize, Serialize};

const NODE_INDEX_URL: &str = "https://nodejs.org/dist/index.json";
const CRATE_API_URL: &str = "https://crates.io/api/v1/crates/nvmman";
const ACCENT: Color = Color::LightCyan;
const ACCENT_DIM: Color = Color::Cyan;
const WARM: Color = Color::LightYellow;
const MUTED: Color = Color::LightBlue;
const SURFACE: Color = Color::DarkGray;
const DANGER: Color = Color::LightRed;
const SCREEN_COUNT: u16 = 5;

fn main() -> Result<()> {
    match env::args().nth(1).as_deref() {
        Some("--help" | "-h") => {
            print_help();
            return Ok(());
        }
        Some("--version" | "-V") => {
            println!("nvmman {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("update") => {
            print_update_status()?;
            return Ok(());
        }
        Some(argument) => bail!("unknown argument: {argument}\n\nRun `nvmman --help` for usage."),
        None => {}
    }
    let manager = Manager::new()?;
    let mut terminal = TerminalGuard::enter()?;
    let result = run_app(&mut terminal.terminal, manager);
    terminal.restore()?;
    result
}

fn print_help() {
    println!(
        "nvmman {}\n\nInteractive nvm and global npm package manager.\n\nUSAGE:\n    nvmman\n    nvmman update\n\nCOMMANDS:\n    update  Check crates.io for a newer nvmman release\n\nSHORTCUTS:\n    r  Refresh state\n    l  Install latest native LTS\n    g  Sync package registry\n    a  Restore registry packages\n    u  Check global npm updates\n    1-5  Switch views\n    q  Quit\n\nThe interactive UI supports mouse selection and mouse-wheel scrolling.",
        env!("CARGO_PKG_VERSION")
    );
}

#[derive(Deserialize)]
struct CrateApiResponse {
    #[serde(rename = "crate")]
    krate: CrateSummary,
}

#[derive(Deserialize)]
struct CrateSummary {
    max_version: String,
}

fn print_update_status() -> Result<()> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let latest = latest_published_version()?;
    println!("{}", update_message(&current, &latest));
    Ok(())
}

fn latest_published_version() -> Result<Version> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("nvmman/{} update-check", env!("CARGO_PKG_VERSION")))
        .build()
        .context("could not create the crates.io client")?;
    let response: CrateApiResponse = client
        .get(CRATE_API_URL)
        .send()
        .context("could not check crates.io for nvmman updates")?
        .error_for_status()
        .context("crates.io returned an error while checking for updates")?
        .json()
        .context("could not decode the crates.io update response")?;
    Version::parse(&response.krate.max_version)
        .context("crates.io returned an invalid nvmman version")
}

fn update_message(current: &Version, latest: &Version) -> String {
    match latest.cmp(current) {
        std::cmp::Ordering::Greater => format!(
            "Update available: nvmman {current} -> {latest}\nRun: cargo install nvmman --version {latest} --force"
        ),
        std::cmp::Ordering::Equal => format!("nvmman {current} is up to date."),
        std::cmp::Ordering::Less => {
            format!("nvmman {current} is newer than the latest crates.io release ({latest}).")
        }
    }
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>, manager: Manager) -> Result<()> {
    let (sender, receiver) = mpsc::channel();
    let mut app = App::new(manager, sender, receiver);
    app.start_task(Task::Refresh);

    while app.running {
        terminal.draw(|frame| render(frame, &mut app))?;

        if event::poll(Duration::from_millis(120))? {
            match event::read()? {
                Event::Key(key) if key.kind == event::KeyEventKind::Press => app.handle_key(key),
                Event::Mouse(mouse) => {
                    let size = terminal.size()?;
                    app.handle_mouse(mouse, Rect::new(0, 0, size.width, size.height));
                }
                _ => {}
            }
        }
        app.receive_worker_events();
    }
    Ok(())
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    restored: bool,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self {
            terminal,
            restored: false,
        })
    }

    fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        disable_raw_mode()?;
        execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            LeaveAlternateScreen
        )?;
        self.terminal.show_cursor()?;
        self.restored = true;
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
struct Package {
    name: String,
    version: String,
}

#[derive(Clone, Debug)]
struct NodeInstallation {
    version: String,
    architecture: String,
    packages: Vec<Package>,
}

#[derive(Clone, Debug)]
struct Snapshot {
    latest_lts: LatestLts,
    host_architecture: String,
    default_node: Option<String>,
    installations: Vec<NodeInstallation>,
    registry_path: PathBuf,
}

#[derive(Clone, Debug)]
struct LatestLts {
    version: String,
    name: String,
    date: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Registry {
    schema_version: u32,
    generated_at: String,
    host_architecture: String,
    scanned_node_versions: Vec<String>,
    source_node_versions: Vec<String>,
    packages: BTreeMap<String, RegistryEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryEntry {
    selected_version: String,
    occurrences: Vec<PackageOccurrence>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageOccurrence {
    version: String,
    node_versions: Vec<String>,
}

#[derive(Clone, Debug)]
struct UpdateCandidate {
    package: Package,
    latest_version: String,
}

#[derive(Deserialize)]
struct NodeRelease {
    version: String,
    lts: serde_json::Value,
    date: String,
}

#[derive(Deserialize)]
struct PackageManifest {
    name: String,
    version: String,
}

#[derive(Clone)]
struct Manager {
    nvm_dir: PathBuf,
}

impl Manager {
    fn new() -> Result<Self> {
        let home = env::var_os("HOME").context("HOME is not set")?;
        let nvm_dir =
            env::var_os("NVM_DIR").map_or_else(|| PathBuf::from(home).join(".nvm"), PathBuf::from);
        if !nvm_dir.join("nvm.sh").is_file() {
            bail!("nvm was not found at {}", nvm_dir.display());
        }
        Ok(Self { nvm_dir })
    }

    fn registry_path(&self) -> PathBuf {
        self.nvm_dir.join("global-packages-registry.json")
    }

    fn snapshot(&self) -> Result<Snapshot> {
        let installations = self.installations()?;
        Ok(Snapshot {
            latest_lts: Self::latest_lts()?,
            host_architecture: Self::host_architecture()?,
            default_node: self.default_node_version()?,
            installations,
            registry_path: self.registry_path(),
        })
    }

    fn latest_lts() -> Result<LatestLts> {
        let releases: Vec<NodeRelease> = reqwest::blocking::get(NODE_INDEX_URL)
            .context("could not fetch the official Node.js release index")?
            .error_for_status()
            .context("Node.js release index returned an error")?
            .json()
            .context("could not decode the Node.js release index")?;
        let release = releases
            .into_iter()
            .find(|release| !release.lts.is_null() && release.lts != serde_json::Value::Bool(false))
            .context("the Node.js release index does not contain an LTS release")?;
        Ok(LatestLts {
            version: release.version,
            name: release.lts.as_str().unwrap_or("LTS").to_owned(),
            date: release.date,
        })
    }

    fn host_architecture() -> Result<String> {
        let machine = command_output(Command::new("uname").arg("-m"))?;
        let translated = Command::new("sysctl")
            .args(["-in", "sysctl.proc_translated"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
        if translated.as_deref() == Some("1") {
            return Ok("arm64".to_owned());
        }
        Ok(normalize_architecture(&machine))
    }

    fn default_node_version(&self) -> Result<Option<String>> {
        let output = self.run_nvm(&["version", "default"])?;
        let version = output.trim();
        if version.is_empty() || version == "N/A" {
            Ok(None)
        } else {
            Ok(Some(version.to_owned()))
        }
    }

    fn installations(&self) -> Result<Vec<NodeInstallation>> {
        let versions_dir = self.nvm_dir.join("versions/node");
        let mut versions = fs::read_dir(&versions_dir)
            .with_context(|| format!("could not read {}", versions_dir.display()))?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|version| version.starts_with('v'))
            .collect::<Vec<_>>();
        versions.sort_by(|left, right| compare_node_versions(left, right));

        versions
            .into_iter()
            .map(|version| {
                let node = self.node_bin(&version);
                let architecture = if node.is_file() {
                    command_output(Command::new(&node).args(["-p", "process.arch"]))
                        .unwrap_or_else(|_| "unknown".to_owned())
                } else {
                    "missing".to_owned()
                };
                Ok(NodeInstallation {
                    packages: self.global_packages(&version)?,
                    version,
                    architecture,
                })
            })
            .collect()
    }

    fn global_packages(&self, version: &str) -> Result<Vec<Package>> {
        let root = self.node_root(version);
        if !root.is_dir() {
            return Ok(Vec::new());
        }
        let mut manifests = Vec::new();
        for entry in
            fs::read_dir(&root).with_context(|| format!("could not read {}", root.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == "npm" || name == "corepack" || !entry.file_type()?.is_dir() {
                continue;
            }
            if name.starts_with('@') {
                for scoped in fs::read_dir(entry.path())? {
                    manifests.push(scoped?.path().join("package.json"));
                }
            } else {
                manifests.push(entry.path().join("package.json"));
            }
        }
        let mut packages = manifests
            .into_iter()
            .filter(|path| path.is_file())
            .map(|path| read_manifest(&path))
            .collect::<Result<Vec<_>>>()?;
        packages.sort();
        Ok(packages)
    }

    fn sync_registry(&self) -> Result<Registry> {
        let installations = self.installations()?;
        let mut packages: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
        for installation in &installations {
            for package in &installation.packages {
                packages
                    .entry(package.name.clone())
                    .or_default()
                    .entry(package.version.clone())
                    .or_default()
                    .insert(installation.version.clone());
            }
        }

        let entries = packages
            .into_iter()
            .map(|(name, versions)| {
                let selected_version = versions
                    .keys()
                    .max_by(|left, right| compare_package_versions(left, right))
                    .cloned()
                    .context("package version map unexpectedly empty")?;
                let occurrences = versions
                    .into_iter()
                    .map(|(version, nodes)| PackageOccurrence {
                        version,
                        node_versions: nodes.into_iter().collect(),
                    })
                    .collect();
                Ok((
                    name,
                    RegistryEntry {
                        selected_version,
                        occurrences,
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;

        let registry = Registry {
            schema_version: 1,
            generated_at: iso_timestamp(),
            host_architecture: Self::host_architecture()?,
            scanned_node_versions: installations
                .iter()
                .map(|node| node.version.clone())
                .collect(),
            source_node_versions: installations
                .iter()
                .filter(|node| !node.packages.is_empty())
                .map(|node| node.version.clone())
                .collect(),
            packages: entries,
        };
        write_json(&self.registry_path(), &registry)?;
        Ok(registry)
    }

    fn load_registry(&self) -> Result<Registry> {
        let path = self.registry_path();
        let data = fs::read_to_string(&path)
            .with_context(|| format!("registry not found at {}", path.display()))?;
        serde_json::from_str(&data).context("registry JSON is invalid")
    }

    fn install_latest_lts(&self) -> Result<String> {
        let latest = Self::latest_lts()?;
        self.run_nvm(&["install", &latest.version])?;
        let architecture = command_output(
            Command::new(self.node_bin(&latest.version)).args(["-p", "process.arch"]),
        )?;
        let expected = Self::host_architecture()?;
        if architecture != expected {
            bail!(
                "nvm installed {architecture} Node, but this machine requires {expected}. Default was not changed."
            );
        }
        self.run_nvm(&["alias", "default", &latest.version])?;
        Ok(format!(
            "Installed {} {} and made it the nvm default.",
            latest.version, expected
        ))
    }

    fn restore_registry(&self) -> Result<String> {
        let registry = self.load_registry()?;
        let version = self.ensure_native_default()?;
        let mut failures = Vec::new();
        for (name, entry) in registry.packages {
            let spec = format!("{name}@{}", entry.selected_version);
            if let Err(error) = self.npm(&version, &["install", "-g", &spec]) {
                failures.push(format!("{spec}: {error:#}"));
            }
        }
        if failures.is_empty() {
            Ok(format!("Restored all registry packages into {version}."))
        } else {
            Ok(format!(
                "Restored packages into {version}; {} package(s) need attention: {}",
                failures.len(),
                failures.join(" | ")
            ))
        }
    }

    fn updates(&self) -> Result<Vec<UpdateCandidate>> {
        let version = self.ensure_native_default()?;
        let mut candidates = Vec::new();
        for package in self.global_packages(&version)? {
            let latest = self.npm(&version, &["view", &package.name, "version"])?;
            let latest = latest.trim();
            if !latest.is_empty() && latest != package.version {
                candidates.push(UpdateCandidate {
                    package,
                    latest_version: latest.to_owned(),
                });
            }
        }
        candidates.sort_by(|left, right| left.package.name.cmp(&right.package.name));
        Ok(candidates)
    }

    fn update_package(&self, candidate: &UpdateCandidate) -> Result<String> {
        let version = self.ensure_native_default()?;
        let spec = format!("{}@{}", candidate.package.name, candidate.latest_version);
        self.npm(&version, &["install", "-g", &spec])?;
        Ok(format!("Updated {} in {version}.", candidate.package.name))
    }

    fn ensure_native_default(&self) -> Result<String> {
        let version = self
            .default_node_version()?
            .context("no nvm default is configured")?;
        let actual =
            command_output(Command::new(self.node_bin(&version)).args(["-p", "process.arch"]))?;
        let expected = Self::host_architecture()?;
        if actual != expected {
            bail!("default {version} is {actual}, but the host requires {expected}");
        }
        Ok(version)
    }

    fn node_bin(&self, version: &str) -> PathBuf {
        self.nvm_dir
            .join("versions/node")
            .join(version)
            .join("bin/node")
    }

    fn node_root(&self, version: &str) -> PathBuf {
        self.nvm_dir
            .join("versions/node")
            .join(version)
            .join("lib/node_modules")
    }

    fn npm(&self, version: &str, args: &[&str]) -> Result<String> {
        command_output(
            Command::new(
                self.nvm_dir
                    .join("versions/node")
                    .join(version)
                    .join("bin/npm"),
            )
            .args(args),
        )
    }

    fn run_nvm(&self, args: &[&str]) -> Result<String> {
        let script = "source \"$0/nvm.sh\"; nvm \"$@\"";
        command_output(
            Command::new("/bin/zsh")
                .arg("-fc")
                .arg(script)
                .arg(&self.nvm_dir)
                .args(args),
        )
    }
}

fn read_manifest(path: &Path) -> Result<Package> {
    let manifest: PackageManifest = serde_json::from_str(&fs::read_to_string(path)?)
        .with_context(|| format!("invalid package manifest at {}", path.display()))?;
    Ok(Package {
        name: manifest.name,
        version: manifest.version,
    })
}

fn write_json(path: &Path, value: &Registry) -> Result<()> {
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        format!("{}\n", serde_json::to_string_pretty(value)?),
    )?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn command_output(command: &mut Command) -> Result<String> {
    let output = command
        .output()
        .with_context(|| format!("could not run {command:?}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!("{command:?} failed: {stderr}")
    }
}

fn normalize_architecture(architecture: &str) -> String {
    match architecture.trim() {
        "arm64" | "aarch64" => "arm64".to_owned(),
        "x86_64" | "amd64" => "x64".to_owned(),
        other => other.to_owned(),
    }
}

fn compare_node_versions(left: &str, right: &str) -> std::cmp::Ordering {
    compare_package_versions(left.trim_start_matches('v'), right.trim_start_matches('v'))
}

fn compare_package_versions(left: &str, right: &str) -> std::cmp::Ordering {
    match (
        Version::parse(left.trim_start_matches('v')),
        Version::parse(right.trim_start_matches('v')),
    ) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        _ => left.cmp(right),
    }
}

fn iso_timestamp() -> String {
    command_output(Command::new("date").args(["-u", "+%Y-%m-%dT%H:%M:%SZ"]))
        .unwrap_or_else(|_| "unknown".to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Dashboard,
    Packages,
    Registry,
    Updates,
    Activity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarHit {
    Screen(usize),
    Action(usize),
}

impl Screen {
    const ALL: [Self; 5] = [
        Self::Dashboard,
        Self::Packages,
        Self::Registry,
        Self::Updates,
        Self::Activity,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::Packages => "Installed packages",
            Self::Registry => "Registry",
            Self::Updates => "Updates",
            Self::Activity => "Activity",
        }
    }
}

#[derive(Clone, Debug)]
enum Task {
    Refresh,
    SyncRegistry,
    InstallLts,
    RestoreRegistry,
    CheckUpdates,
    UpdatePackage(UpdateCandidate),
}

#[derive(Debug)]
enum WorkerResult {
    Snapshot(Snapshot),
    Synced {
        snapshot: Snapshot,
        registry: Registry,
        message: String,
    },
    Updates(Vec<UpdateCandidate>),
    Message {
        snapshot: Snapshot,
        message: String,
    },
}

#[derive(Debug)]
struct WorkerEvent {
    task: Task,
    result: Result<WorkerResult, String>,
}

struct App {
    manager: Manager,
    sender: Sender<WorkerEvent>,
    receiver: Receiver<WorkerEvent>,
    running: bool,
    busy: bool,
    screen: Screen,
    selected_view: usize,
    snapshot: Option<Snapshot>,
    registry: Option<Registry>,
    updates: Vec<UpdateCandidate>,
    selected_update: usize,
    scroll: u16,
    modal: Option<Task>,
    status: String,
    activity: Vec<String>,
}

impl App {
    fn new(manager: Manager, sender: Sender<WorkerEvent>, receiver: Receiver<WorkerEvent>) -> Self {
        Self {
            manager,
            sender,
            receiver,
            running: true,
            busy: false,
            screen: Screen::Dashboard,
            selected_view: 0,
            snapshot: None,
            registry: None,
            updates: Vec::new(),
            selected_update: 0,
            scroll: 0,
            modal: None,
            status: "Starting nvmman...".to_owned(),
            activity: Vec::new(),
        }
    }

    fn start_task(&mut self, task: Task) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.status = format!("Running {}...", task_label(&task));
        self.activity.push(self.status.clone());
        let manager = self.manager.clone();
        let sender = self.sender.clone();
        thread::spawn(move || {
            let result = run_task(&manager, &task).map_err(|error| format!("{error:#}"));
            let _ = sender.send(WorkerEvent { task, result });
        });
    }

    fn receive_worker_events(&mut self) {
        while let Ok(event) = self.receiver.try_recv() {
            self.busy = false;
            match event.result {
                Ok(WorkerResult::Snapshot(snapshot)) => {
                    self.status.clear();
                    self.status.push_str("Machine state refreshed.");
                    self.snapshot = Some(snapshot);
                }
                Ok(WorkerResult::Synced {
                    snapshot,
                    registry,
                    message,
                }) => {
                    self.status = message;
                    self.snapshot = Some(snapshot);
                    self.registry = Some(registry);
                }
                Ok(WorkerResult::Updates(updates)) => {
                    self.status = format!("{} package update(s) available.", updates.len());
                    self.updates = updates;
                    self.selected_update = 0;
                    self.screen = Screen::Updates;
                }
                Ok(WorkerResult::Message { snapshot, message }) => {
                    self.status = message;
                    self.snapshot = Some(snapshot);
                }
                Err(error) => self.status = format!("{} failed: {error}", task_label(&event.task)),
            }
            self.activity.push(self.status.clone());
            self.activity.truncate(120);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if let Some(task) = self.modal.clone() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.modal = None;
                    self.start_task(task);
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.modal = None;
                    self.status.clear();
                    self.status.push_str("Action cancelled.");
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.running = false,
            KeyCode::Char('r') => self.start_task(Task::Refresh),
            KeyCode::Char('g') => self.start_task(Task::SyncRegistry),
            KeyCode::Char('l') => self.confirm(Task::InstallLts),
            KeyCode::Char('a') => self.confirm(Task::RestoreRegistry),
            KeyCode::Char('u') => self.start_task(Task::CheckUpdates),
            KeyCode::Char('1') => self.select_screen(Screen::Dashboard),
            KeyCode::Char('2') => self.select_screen(Screen::Packages),
            KeyCode::Char('3') => self.select_screen(Screen::Registry),
            KeyCode::Char('4') => self.select_screen(Screen::Updates),
            KeyCode::Char('5') => self.select_screen(Screen::Activity),
            KeyCode::Down => self.navigate_down(),
            KeyCode::Up => self.navigate_up(),
            KeyCode::Char('j') | KeyCode::PageDown => self.scroll_content(8),
            KeyCode::Char('k') | KeyCode::PageUp => self.scroll_content(-8),
            KeyCode::Enter if self.screen == Screen::Updates => {
                if let Some(candidate) = self.updates.get(self.selected_update).cloned() {
                    self.confirm(Task::UpdatePackage(candidate));
                }
            }
            _ => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) {
        match mouse.kind {
            MouseEventKind::ScrollDown => self.scroll_content(3),
            MouseEventKind::ScrollUp => self.scroll_content(-3),
            MouseEventKind::Down(MouseButton::Left) => {
                if self.modal.is_some() {
                    let dialog = centered_rect(60, 28, area);
                    if mouse.row >= dialog.y + dialog.height.saturating_sub(3) {
                        if mouse.column < dialog.x + dialog.width / 2 {
                            self.handle_key(KeyEvent::new(
                                KeyCode::Enter,
                                event::KeyModifiers::NONE,
                            ));
                        } else {
                            self.handle_key(KeyEvent::new(KeyCode::Esc, event::KeyModifiers::NONE));
                        }
                    }
                    return;
                }
                self.handle_click(mouse.column, mouse.row, area);
            }
            _ => {}
        }
    }

    fn handle_click(&mut self, column: u16, row: u16, area: Rect) {
        let (sidebar, content) = main_columns(area);
        if let Some(hit) = sidebar_hit(sidebar, column, row) {
            match hit {
                SidebarHit::Screen(index) => self.select_screen(Screen::ALL[index]),
                SidebarHit::Action(0) => self.start_task(Task::Refresh),
                SidebarHit::Action(1) => self.confirm(Task::InstallLts),
                SidebarHit::Action(2) => self.start_task(Task::SyncRegistry),
                SidebarHit::Action(3) => self.confirm(Task::RestoreRegistry),
                SidebarHit::Action(4) => self.start_task(Task::CheckUpdates),
                SidebarHit::Action(_) => {}
            }
        } else if self.screen == Screen::Updates
            && column >= content.x
            && column < content.right()
            && let Some(index) = update_row_at(content, row)
            && let Some(candidate) = self.updates.get(index).cloned()
        {
            self.selected_update = index;
            self.confirm(Task::UpdatePackage(candidate));
        }
    }

    fn confirm(&mut self, task: Task) {
        if !self.busy {
            self.modal = Some(task);
        }
    }

    fn select_screen(&mut self, screen: Screen) {
        self.screen = screen;
        self.selected_view = Screen::ALL
            .iter()
            .position(|candidate| *candidate == screen)
            .unwrap_or_default();
        self.scroll = 0;
        if screen == Screen::Registry && self.registry.is_none() {
            self.registry = self.manager.load_registry().ok();
        }
    }

    fn navigate_down(&mut self) {
        if self.screen == Screen::Updates && !self.updates.is_empty() {
            self.selected_update = self
                .selected_update
                .saturating_add(1)
                .min(self.updates.len().saturating_sub(1));
        } else {
            self.selected_view = (self.selected_view + 1) % Screen::ALL.len();
            self.select_screen(Screen::ALL[self.selected_view]);
        }
    }

    fn navigate_up(&mut self) {
        if self.screen == Screen::Updates && !self.updates.is_empty() {
            self.selected_update = self.selected_update.saturating_sub(1);
        } else {
            self.selected_view = self
                .selected_view
                .checked_sub(1)
                .unwrap_or(Screen::ALL.len() - 1);
            self.select_screen(Screen::ALL[self.selected_view]);
        }
    }

    fn scroll_content(&mut self, delta: i16) {
        if self.screen == Screen::Updates && !self.updates.is_empty() {
            if delta.is_positive() {
                self.selected_update = self
                    .selected_update
                    .saturating_add(usize::from(delta.unsigned_abs()))
                    .min(self.updates.len().saturating_sub(1));
            } else {
                self.selected_update = self
                    .selected_update
                    .saturating_sub(usize::from(delta.unsigned_abs()));
            }
        } else if delta.is_positive() {
            self.scroll = self.scroll.saturating_add(delta.unsigned_abs());
        } else {
            self.scroll = self.scroll.saturating_sub(delta.unsigned_abs());
        }
    }
}

fn run_task(manager: &Manager, task: &Task) -> Result<WorkerResult> {
    match task {
        Task::Refresh => Ok(WorkerResult::Snapshot(manager.snapshot()?)),
        Task::SyncRegistry => {
            let registry = manager.sync_registry()?;
            let package_count = registry.packages.len();
            Ok(WorkerResult::Synced {
                snapshot: manager.snapshot()?,
                registry,
                message: format!("Registry synced: {package_count} packages."),
            })
        }
        Task::InstallLts => {
            let message = manager.install_latest_lts()?;
            let _ = manager.sync_registry();
            Ok(WorkerResult::Message {
                snapshot: manager.snapshot()?,
                message,
            })
        }
        Task::RestoreRegistry => {
            let message = manager.restore_registry()?;
            let registry = manager.sync_registry()?;
            Ok(WorkerResult::Synced {
                snapshot: manager.snapshot()?,
                registry,
                message,
            })
        }
        Task::CheckUpdates => Ok(WorkerResult::Updates(manager.updates()?)),
        Task::UpdatePackage(candidate) => {
            let message = manager.update_package(candidate)?;
            let registry = manager.sync_registry()?;
            Ok(WorkerResult::Synced {
                snapshot: manager.snapshot()?,
                registry,
                message,
            })
        }
    }
}

fn task_label(task: &Task) -> &'static str {
    match task {
        Task::Refresh => "Refresh",
        Task::SyncRegistry => "Registry sync",
        Task::InstallLts => "Latest LTS installation",
        Task::RestoreRegistry => "Registry restore",
        Task::CheckUpdates => "Update check",
        Task::UpdatePackage(_) => "Package update",
    }
}

fn main_columns(area: Rect) -> (Rect, Rect) {
    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .split(area)[1];
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(30), Constraint::Min(30)])
        .split(main);
    (columns[0], columns[1])
}

fn sidebar_rows(area: Rect) -> (Rect, Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8),
            Constraint::Length(9),
            Constraint::Min(0),
        ])
        .split(area);
    (rows[0], rows[1])
}

fn sidebar_hit(sidebar: Rect, column: u16, row: u16) -> Option<SidebarHit> {
    if column < sidebar.x || column >= sidebar.right() {
        return None;
    }
    let (views, actions) = sidebar_rows(sidebar);
    let view_content = Block::default().borders(Borders::ALL).inner(views);
    if row >= view_content.y && row < view_content.bottom() {
        let index = usize::from(row - view_content.y);
        if index < usize::from(SCREEN_COUNT) {
            return Some(SidebarHit::Screen(index));
        }
    }
    let action_content = Block::default().borders(Borders::ALL).inner(actions);
    if row >= action_content.y && row < action_content.bottom() {
        let index = usize::from(row - action_content.y);
        if index < 5 {
            return Some(SidebarHit::Action(index));
        }
    }
    None
}

fn update_row_at(content: Rect, row: u16) -> Option<usize> {
    let inner = Block::default().borders(Borders::ALL).inner(content);
    let first_data_row = inner.y.saturating_add(1);
    row.checked_sub(first_data_row).map(usize::from)
}

fn render(frame: &mut ratatui::Frame, app: &mut App) {
    let area = frame.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .split(area);
    render_header(frame, layout[0], app);
    let (sidebar, content) = main_columns(area);
    render_sidebar(frame, sidebar, app);
    render_content(frame, content, app);
    render_footer(frame, layout[2], app);
    if let Some(task) = &app.modal {
        render_confirmation(frame, area, task);
    }
}

fn render_header(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let badge = if app.busy { " WORKING " } else { " READY " };
    let title = Line::from(vec![
        Span::styled(
            " nvmman ",
            Style::default()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  native Node + npm maintenance",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {badge}"),
            Style::default().fg(if app.busy { WARM } else { ACCENT }),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(title).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(ACCENT_DIM)),
        ),
        area,
    );
}

fn render_sidebar(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let (views, actions_area) = sidebar_rows(area);
    let navigation = Screen::ALL
        .iter()
        .enumerate()
        .map(|(index, screen)| ListItem::new(format!(" {}  {}", index + 1, screen.title())))
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select(Some(app.selected_view));
    frame.render_stateful_widget(
        List::new(navigation)
            .block(
                Block::default()
                    .title(" Views ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ACCENT_DIM)),
            )
            .highlight_style(
                Style::default()
                    .bg(ACCENT_DIM)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        views,
        &mut state,
    );
    let actions = [
        ("r", "Refresh state"),
        ("l", "Install latest LTS"),
        ("g", "Sync registry"),
        ("a", "Restore registry"),
        ("u", "Check updates"),
    ];
    let items = actions
        .iter()
        .map(|(key, label)| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {key} "),
                    Style::default().fg(Color::Black).bg(WARM),
                ),
                Span::styled(format!(" {label}"), Style::default().fg(Color::White)),
            ]))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(" Actions ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(WARM)),
        ),
        actions_area,
    );
}

fn render_content(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    match app.screen {
        Screen::Dashboard => render_dashboard(frame, area, app),
        Screen::Packages => render_packages(frame, area, app),
        Screen::Registry => render_registry(frame, area, app),
        Screen::Updates => render_updates(frame, area, app),
        Screen::Activity => render_activity(frame, area, app),
    }
}

fn render_dashboard(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let Some(snapshot) = &app.snapshot else {
        frame.render_widget(
            Paragraph::new("Loading machine state...").block(content_block(" Dashboard ")),
            area,
        );
        return;
    };
    let default = snapshot.default_node.as_deref().unwrap_or("not configured");
    let default_arch = snapshot
        .installations
        .iter()
        .find(|node| node.version == default)
        .map_or("unknown", |node| node.architecture.as_str());
    let status = if default_arch == snapshot.host_architecture {
        "native"
    } else {
        "check architecture"
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("Latest LTS  ", Style::default().fg(MUTED)),
            Span::styled(
                format!(
                    "{}  {}  {}",
                    snapshot.latest_lts.version, snapshot.latest_lts.name, snapshot.latest_lts.date
                ),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("nvm default ", Style::default().fg(MUTED)),
            Span::styled(
                default,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Architecture", Style::default().fg(MUTED)),
            Span::styled(
                format!(
                    " host: {}  default: {}  [{}]",
                    snapshot.host_architecture, default_arch, status
                ),
                Style::default().fg(if status == "native" { ACCENT } else { DANGER }),
            ),
        ]),
        Line::raw(""),
        Line::from(vec![Span::styled(
            "Installed nvm Node versions",
            Style::default().fg(WARM).add_modifier(Modifier::BOLD),
        )]),
        Line::from(
            snapshot
                .installations
                .iter()
                .map(|node| {
                    format!(
                        "{} ({}, {} packages)",
                        node.version,
                        node.architecture,
                        node.packages.len()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Line::raw(""),
        Line::from(vec![
            Span::styled("Registry  ", Style::default().fg(MUTED)),
            Span::raw(snapshot.registry_path.display().to_string()),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(content_block(" Dashboard "))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_packages(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let Some(snapshot) = &app.snapshot else {
        return;
    };
    let mut rows = Vec::new();
    for node in &snapshot.installations {
        if node.packages.is_empty() {
            rows.push(Row::new(vec![
                node.version.clone(),
                node.architecture.clone(),
                "-".to_owned(),
            ]));
        } else {
            for package in &node.packages {
                rows.push(Row::new(vec![
                    node.version.clone(),
                    node.architecture.clone(),
                    format!("{}@{}", package.name, package.version),
                ]));
            }
        }
    }
    let total_rows = rows.len();
    let table = Table::new(
        rows.into_iter().skip(usize::from(app.scroll)),
        [
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(["Node", "Arch", "Global package"])
            .style(Style::default().fg(WARM).add_modifier(Modifier::BOLD)),
    )
    .block(content_block(" Every installed nvm Node "))
    .row_highlight_style(Style::default().bg(SURFACE).fg(Color::White));
    frame.render_widget(table, area);
    render_scrollbar(frame, area, app.scroll, saturating_u16(total_rows));
}

fn render_registry(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let Some(registry) = &app.registry else {
        frame.render_widget(
            Paragraph::new("No registry loaded. Press g to scan every installed nvm Node.")
                .block(content_block(" Consolidated registry ")),
            area,
        );
        return;
    };
    let mut lines = vec![
        Line::from(format!("Generated: {}", registry.generated_at)).fg(MUTED),
        Line::from(format!(
            "Scanned: {}",
            registry.scanned_node_versions.join(", ")
        ))
        .fg(MUTED),
        Line::from(format!("Packages: {}", registry.packages.len())).fg(WARM),
        Line::raw(""),
    ];
    for (name, entry) in &registry.packages {
        let seen = entry
            .occurrences
            .iter()
            .map(|occurrence| {
                format!(
                    "{} on {}",
                    occurrence.version,
                    occurrence.node_versions.join(",")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        lines.push(Line::from(vec![
            Span::styled(
                format!("{name}@{}", entry.selected_version),
                Style::default().fg(ACCENT),
            ),
            Span::styled(format!("  [{seen}]"), Style::default().fg(MUTED)),
        ]));
    }
    let paragraph = Paragraph::new(lines)
        .scroll((app.scroll, 0))
        .block(content_block(" Consolidated registry "));
    frame.render_widget(paragraph, area);
    render_scrollbar(
        frame,
        area,
        app.scroll,
        saturating_u16(registry.packages.len()).saturating_add(4),
    );
}

fn render_updates(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    if app.updates.is_empty() {
        frame.render_widget(Paragraph::new("Press u to query npm for global package updates. Each update is confirmed individually.").block(content_block(" Available updates ")).wrap(Wrap { trim: false }), area);
        return;
    }
    let rows = app
        .updates
        .iter()
        .map(|candidate| {
            Row::new(vec![
                candidate.package.name.clone(),
                candidate.package.version.clone(),
                candidate.latest_version.clone(),
            ])
        })
        .collect::<Vec<_>>();
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(55),
            Constraint::Percentage(22),
            Constraint::Percentage(23),
        ],
    )
    .header(
        Row::new(["Package", "Installed", "Latest"])
            .style(Style::default().fg(WARM).add_modifier(Modifier::BOLD)),
    )
    .block(content_block(
        " Available updates - Enter or click, then confirm ",
    ))
    .row_highlight_style(
        Style::default()
            .bg(SURFACE)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    let mut state = ratatui::widgets::TableState::default();
    state.select(Some(app.selected_update));
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_activity(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let lines = app
        .activity
        .iter()
        .rev()
        .map(|entry| Line::from(entry.clone()))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.scroll, 0))
            .block(content_block(" Activity "))
            .wrap(Wrap { trim: false }),
        area,
    );
    render_scrollbar(frame, area, app.scroll, saturating_u16(app.activity.len()));
}

fn render_footer(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let status = if app.busy {
        format!("{}  Please wait", app.status)
    } else {
        app.status.clone()
    };
    let footer = Line::from(vec![
        Span::styled(" q ", Style::default().fg(Color::Black).bg(WARM)),
        Span::raw(" quit  "),
        Span::styled(" arrows/jk ", Style::default().fg(ACCENT)),
        Span::raw(" navigate  "),
        Span::styled(" mouse ", Style::default().fg(ACCENT)),
        Span::raw(" click/scroll  "),
        Span::styled(status, Style::default().fg(MUTED)),
    ]);
    frame.render_widget(
        Paragraph::new(footer)
            .style(Style::default().fg(Color::White))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(ACCENT_DIM)),
            ),
        area,
    );
}

fn render_confirmation(frame: &mut ratatui::Frame, area: Rect, task: &Task) {
    let dialog = centered_rect(60, 28, area);
    frame.render_widget(Clear, dialog);
    let body = match task {
        Task::InstallLts => {
            "Install the official latest LTS, verify its architecture, and make it the nvm default?"
        }
        Task::RestoreRegistry => {
            "Install every registry package into the current default Node? Existing packages are not removed first."
        }
        Task::UpdatePackage(candidate) => {
            return render_package_confirmation(frame, dialog, candidate);
        }
        _ => "Run this action?",
    };
    let text = Text::from(vec![
        Line::from(Span::styled(
            "Confirm action",
            Style::default().fg(WARM).add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::from(body),
        Line::raw(""),
        Line::from(vec![
            Span::styled(" Yes ", Style::default().fg(Color::Black).bg(ACCENT)),
            Span::raw("  Enter / y      "),
            Span::styled(" No ", Style::default().fg(Color::Black).bg(WARM)),
            Span::raw("  Esc / n"),
        ]),
    ]);
    frame.render_widget(
        Paragraph::new(text)
            .block(
                Block::default()
                    .title(" nvmman ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ACCENT)),
            )
            .wrap(Wrap { trim: false }),
        dialog,
    );
}

fn render_package_confirmation(
    frame: &mut ratatui::Frame,
    dialog: Rect,
    candidate: &UpdateCandidate,
) {
    let text = Text::from(vec![
        Line::from(Span::styled(
            "Confirm package update",
            Style::default().fg(WARM).add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::from(format!(
            "{}: {} -> {}",
            candidate.package.name, candidate.package.version, candidate.latest_version
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled(" Yes ", Style::default().fg(Color::Black).bg(ACCENT)),
            Span::raw("  Enter / y      "),
            Span::styled(" No ", Style::default().fg(Color::Black).bg(WARM)),
            Span::raw("  Esc / n"),
        ]),
    ]);
    frame.render_widget(
        Paragraph::new(text)
            .block(
                Block::default()
                    .title(" nvmman ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ACCENT)),
            )
            .wrap(Wrap { trim: false }),
        dialog,
    );
}

fn render_scrollbar(frame: &mut ratatui::Frame, area: Rect, position: u16, content_length: u16) {
    if content_length > area.height.saturating_sub(2) {
        let mut state = ScrollbarState::new(content_length as usize).position(position as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            area.inner(ratatui::layout::Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut state,
        );
    }
}

fn content_block(title: &str) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT_DIM))
        .style(Style::default().fg(Color::White))
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn saturating_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_comparison_prefers_newer_stable_release() {
        assert!(compare_package_versions("1.10.0", "1.9.9").is_gt());
        assert!(compare_package_versions("1.0.0", "1.0.0-rc.1").is_gt());
    }

    #[test]
    fn architecture_normalization_matches_node_names() {
        assert_eq!(normalize_architecture("aarch64"), "arm64");
        assert_eq!(normalize_architecture("x86_64"), "x64");
    }

    #[test]
    fn registry_serializes_with_documented_field_names() {
        let registry = Registry {
            schema_version: 1,
            generated_at: "now".to_owned(),
            host_architecture: "arm64".to_owned(),
            scanned_node_versions: vec!["v24.19.0".to_owned()],
            source_node_versions: vec!["v24.19.0".to_owned()],
            packages: BTreeMap::new(),
        };
        let json = serde_json::to_string(&registry).expect("registry serializes");
        assert!(json.contains("schemaVersion"));
        assert!(json.contains("scannedNodeVersions"));
    }

    #[test]
    fn sidebar_hit_testing_uses_the_visible_bordered_regions() {
        let sidebar = Rect::new(0, 3, 30, 20);
        let (views, actions) = sidebar_rows(sidebar);
        let first_view_row = Block::default().borders(Borders::ALL).inner(views).y;
        let first_action_row = Block::default().borders(Borders::ALL).inner(actions).y;

        assert_eq!(
            sidebar_hit(sidebar, 4, first_view_row),
            Some(SidebarHit::Screen(0))
        );
        assert_eq!(
            sidebar_hit(sidebar, 4, first_view_row + 4),
            Some(SidebarHit::Screen(4))
        );
        assert_eq!(
            sidebar_hit(sidebar, 4, first_action_row),
            Some(SidebarHit::Action(0))
        );
        assert_eq!(
            sidebar_hit(sidebar, 4, first_action_row + 4),
            Some(SidebarHit::Action(4))
        );
    }

    #[test]
    fn update_rows_start_after_the_table_header() {
        let content = Rect::new(30, 3, 80, 20);
        let inner = Block::default().borders(Borders::ALL).inner(content);
        assert_eq!(update_row_at(content, inner.y + 1), Some(0));
        assert_eq!(update_row_at(content, inner.y + 4), Some(3));
    }

    #[test]
    fn update_message_offers_an_exact_install_command() {
        let current = Version::parse("0.1.2").expect("valid current version");
        let latest = Version::parse("0.1.3").expect("valid latest version");
        assert_eq!(
            update_message(&current, &latest),
            "Update available: nvmman 0.1.2 -> 0.1.3\nRun: cargo install nvmman --version 0.1.3 --force"
        );
    }

    #[test]
    fn update_message_reports_current_versions() {
        let version = Version::parse("0.1.3").expect("valid version");
        assert_eq!(
            update_message(&version, &version),
            "nvmman 0.1.3 is up to date."
        );
    }
}
