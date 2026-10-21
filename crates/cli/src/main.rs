//! Command-line test bench for the engine.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use facteur_core::{Draft, Engine, FlagKind, NewAccount, NewAuth, OAuthProvider, Security, ServerConfig, ThreadFilter};

#[derive(Parser)]
#[command(name = "facteur-cli", about = "Test bench for the facteur engine")]
struct Cli {
    /// Data folder (default: ~/.local/share/facteur).
    #[arg(long, global = true)]
    data: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Adds an account. Servers as host:port:security (tls, starttls, none).
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        email: String,
        #[arg(long)]
        user: String,
        #[arg(long)]
        pass: String,
        #[arg(long)]
        imap: String,
        #[arg(long)]
        smtp: String,
    },
    /// Looks up an address's servers without saving anything.
    Discover { email: String },
    /// Transcribes a 16 kHz mono 16-bit WAV file with a whisper model (build with `--features local-ai`).
    Hear {
        model: std::path::PathBuf,
        wav: std::path::PathBuf,
        #[arg(long)]
        language: Option<String>,
    },
    /// Asks a local model (GGUF file) a question, the system instructions and the text read from
    /// files: to try models and instructions (build with `--features local-ai`).
    Ai {
        model: std::path::PathBuf,
        #[arg(long)]
        system: std::path::PathBuf,
        #[arg(long)]
        input: std::path::PathBuf,
        #[arg(long, default_value_t = 400)]
        max: usize,
        /// GBNF grammar file constraining the answer.
        #[arg(long)]
        grammar: Option<std::path::PathBuf>,
    },
    /// Adds an account through OAuth2 (google or microsoft): prints the URL to open and waits for the redirect.
    Oauth {
        provider: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        imap: String,
        #[arg(long)]
        smtp: String,
    },
    Accounts,
    Sync { account: i64 },
    Flush { account: i64 },
    Boxes { account: i64 },
    Threads { mailbox: i64 },
    Thread { account: i64, thread: i64 },
    Read { id: i64 },
    Flag { id: i64, flag: String, value: String },
    Archive { id: i64 },
    Trash { id: i64 },
    Search { account: i64, query: String },
    Send {
        account: i64,
        #[arg(long)]
        to: String,
        #[arg(long)]
        subject: String,
        #[arg(long)]
        body: String,
        #[arg(long)]
        reply: Option<i64>,
    },
}

fn server(s: &str) -> Result<ServerConfig, String> {
    let mut parts = s.rsplitn(3, ':');
    let security = parts.next().ok_or("missing security")?;
    let port = parts.next().ok_or("missing port")?.parse().map_err(|_| "invalid port")?;
    let host = parts.next().ok_or("missing host")?.to_string();
    let security = match security {
        "tls" => Security::Tls,
        "starttls" => Security::StartTls,
        "none" => Security::None,
        other => return Err(format!("unknown security: {other}")),
    };
    Ok(ServerConfig { host, port, security })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    let data = cli
        .data
        .unwrap_or_else(|| dirs::data_dir().expect("no data folder").join("facteur"));
    let engine = Engine::open(&data)?;

    match cli.cmd {
        Cmd::Add { name, email, user, pass, imap, smtp } => {
            let account = engine
                .add_account(NewAccount { name, email, username: user, password: pass, imap: server(&imap)?, smtp: server(&smtp)?, auth: NewAuth::default() })
                .await?;
            println!("account {} added", account.id);
        }
        Cmd::Oauth { provider, name, imap, smtp } => {
            let provider = OAuthProvider::parse(&provider).ok_or("unknown provider: google or microsoft")?;
            let identity = engine
                .oauth_login(provider, None, false, |url| {
                    println!("Open this address in a browser:\n{url}");
                    Ok(())
                })
                .await?;
            let email = identity.email.clone().ok_or("no address in the identity token")?;
            let account = engine
                .add_account(NewAccount {
                    name,
                    username: email.clone(),
                    email,
                    password: String::new(),
                    imap: server(&imap)?,
                    smtp: server(&smtp)?,
                    auth: NewAuth::OAuth2 { provider, ticket: identity.ticket },
                })
                .await?;
            println!("account {} added", account.id);
        }
        Cmd::Hear { model, wav, language } => {
            let bytes = std::fs::read(wav)?;
            // The data after a canonical 44-byte header: 16-bit little-endian samples.
            let samples: Vec<f32> = bytes[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
            let started = std::time::Instant::now();
            println!("{}", facteur_core::ai::transcribe(&model, &samples, language.as_deref())?);
            eprintln!("({:.1} s for {:.1} s of audio)", started.elapsed().as_secs_f32(), samples.len() as f32 / 16000.0);
        }
        Cmd::Ai { model, system, input, max, grammar } => {
            let system = std::fs::read_to_string(system)?;
            let input = std::fs::read_to_string(input)?;
            let grammar = grammar.map(std::fs::read_to_string).transpose()?;
            let turns = [facteur_core::ai::Turn { role: "system", content: &system }, facteur_core::ai::Turn { role: "user", content: &input }];
            let started = std::time::Instant::now();
            let answer = facteur_core::ai::generate(&model, &turns, max, grammar.as_deref())?;
            println!("{}", facteur_core::ai::without_thinking(&answer));
            eprintln!("({:.1} s)", started.elapsed().as_secs_f32());
        }
        Cmd::Discover { email } => {
            let found = facteur_core::discover(&email).await?;
            println!("{}", serde_json::to_string_pretty(&found)?);
        }
        Cmd::Accounts => {
            for a in engine.accounts()? {
                println!("{:>3}  {} <{}>  imap {}:{}", a.id, a.name, a.email, a.imap.host, a.imap.port);
            }
        }
        Cmd::Sync { account } => {
            let r = engine.sync(account).await?;
            println!("{} new messages ({} in INBOX), {} operations replayed", r.new_messages, r.new_in_inbox, r.flushed_ops);
        }
        Cmd::Flush { account } => println!("{} operations replayed", engine.flush(account).await?),
        Cmd::Boxes { account } => {
            for b in engine.mailboxes(account)? {
                let role = b.role.map(|r| r.as_str()).unwrap_or("-");
                println!("{:>3}  {:<20} {:<8} {}/{}", b.id, b.display_name, role, b.unread, b.total);
            }
        }
        Cmd::Threads { mailbox } => {
            for t in engine.threads(mailbox, ThreadFilter::All, None, 50, 0)? {
                let mark = if t.unread > 0 { '●' } else { ' ' };
                println!(
                    "{mark} thread {:>4}  latest {:>4}  ({})  {:<30}  {}  — {}",
                    t.thread_id, t.latest_id, t.count, t.participants.iter().map(|p| p.name.clone().unwrap_or_else(|| p.email.clone())).collect::<Vec<_>>().join(", "), t.subject, t.preview
                );
            }
        }
        Cmd::Thread { account, thread } => {
            for m in engine.thread(account, thread, None)? {
                let from = m.from.map(|a| a.email).unwrap_or_default();
                println!("{:>4}  [folder {}]  {}  {:<25} {}", m.id, m.mailbox_id, m.date, from, m.subject);
            }
        }
        Cmd::Read { id } => {
            let m = engine.message(id).await?;
            println!("From    : {:?}", m.summary.from);
            println!("To      : {:?}", m.summary.to);
            println!("Subject : {}", m.summary.subject);
            println!("Remote  : {}", m.has_remote_content);
            for a in &m.attachments {
                println!("Att #{} : {} ({}, {} bytes)", a.index, a.filename, a.mime, a.size);
            }
            println!("\n{}", m.text.unwrap_or_else(|| m.html.clone()));
        }
        Cmd::Flag { id, flag, value } => {
            let flag = match flag.as_str() {
                "seen" => FlagKind::Seen,
                "flagged" => FlagKind::Flagged,
                f => return Err(format!("unknown flag: {f}").into()),
            };
            engine.set_flag(&[id], flag, value == "on")?;
            println!("{} operations replayed", engine.flush(engine.account_of(id)?).await?);
        }
        Cmd::Archive { id } => {
            let account = engine.account_of(id)?;
            engine.archive(&[id])?;
            println!("{} operations replayed", engine.flush(account).await?);
        }
        Cmd::Trash { id } => {
            let account = engine.account_of(id)?;
            engine.trash(&[id], true)?;
            println!("{} operations replayed", engine.flush(account).await?);
        }
        Cmd::Search { account, query } => {
            for m in engine.search(account, &query, false)? {
                println!("{:>4}  {}  {}", m.id, m.subject, m.preview);
            }
        }
        Cmd::Send { account, to, subject, body, reply } => {
            engine
                .send(Draft {
                    account_id: account,
                    to: to.split(',').map(str::to_string).collect(),
                    subject,
                    body,
                    in_reply_to: reply,
                    ..Default::default()
                }, None, None)
                .await?;
            println!("sent; {} operations replayed", engine.flush(account).await?);
        }
    }
    Ok(())
}
