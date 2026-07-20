//! Управление Python-сайдкаром (ML): запуск процесса и JSON-RPC по stdin/stdout.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

const INSTALL_HINT: &str =
    "\n\nУстанови зависимости ИИ:\n  python -m pip install -r sidecar\\requirements.txt";

pub struct Sidecar {
    inner: Mutex<Option<Proc>>,
}

struct Proc {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Sidecar {
    pub fn new() -> Self {
        Sidecar {
            inner: Mutex::new(None),
        }
    }

    fn spawn(script: &Path, python: &str) -> Result<Proc, String> {
        // Пустой python = script на самом деле собранный бинарник сайдкара
        let packaged = python.is_empty();
        let candidates: Vec<Vec<String>> = if packaged {
            vec![Vec::new()] // без интерпретатора
        } else {
            let mut c: Vec<Vec<String>> = vec![vec![python.to_string()]];
            if python != "python" {
                c.push(vec!["python".into()]);
            }
            c.push(vec!["py".into(), "-3".into()]);
            c
        };

        let mut last_err = String::new();
        for cand in candidates {
            let mut cmd = if packaged {
                Command::new(script)
            } else {
                let mut c = Command::new(&cand[0]);
                for a in &cand[1..] {
                    c.arg(a);
                }
                c.arg(script);
                c
            };
            cmd
                .env("PYTHONIOENCODING", "utf-8") // русская Windows: пайпы иначе в cp1251
                .env("PYTHONUTF8", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped());
            // В dev логи сайдкара видны в консоли; в релизе консоли нет —
            // отдаём stderr в никуда, иначе Python может падать на выводе.
            #[cfg(debug_assertions)]
            cmd.stderr(Stdio::inherit());
            #[cfg(not(debug_assertions))]
            cmd.stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            }
            match cmd.spawn() {
                Ok(mut child) => {
                    let stdin = child.stdin.take().ok_or("нет stdin у сайдкара")?;
                    let stdout =
                        BufReader::new(child.stdout.take().ok_or("нет stdout у сайдкара")?);
                    return Ok(Proc {
                        child,
                        stdin,
                        stdout,
                        next_id: 1,
                    });
                }
                Err(e) => last_err = e.to_string(),
            }
        }
        if packaged {
            return Err(format!("Не удалось запустить собранный сайдкар: {last_err}"));
        }
        Err(format!(
            "Не удалось запустить Python ({last_err}). Установи Python 3.11+{INSTALL_HINT}"
        ))
    }

    /// Вызов метода сайдкара. Блокирующий — вызывать из spawn_blocking.
    /// Mutex сериализует запросы; при ошибке ввода-вывода процесс перезапускается
    /// при следующем вызове.
    pub fn rpc(&self, script: &Path, python: &str, method: &str, params: Value) -> Result<Value, String> {
        let mut guard = self.inner.lock().unwrap();
        if guard.is_none() {
            *guard = Some(Self::spawn(script, python)?);
        }
        let proc = guard.as_mut().unwrap();
        let id = proc.next_id;
        proc.next_id += 1;
        let req = json!({"id": id, "method": method, "params": params}).to_string() + "\n";

        match Self::rpc_inner(proc, id, &req) {
            Ok(v) => Ok(v),
            Err((msg, fatal)) => {
                if fatal {
                    if let Some(mut p) = guard.take() {
                        let _ = p.child.kill();
                    }
                }
                Err(msg)
            }
        }
    }

    fn rpc_inner(proc: &mut Proc, id: u64, req: &str) -> Result<Value, (String, bool)> {
        proc.stdin
            .write_all(req.as_bytes())
            .and_then(|_| proc.stdin.flush())
            .map_err(|e| (format!("ИИ-модуль недоступен: {e}{INSTALL_HINT}"), true))?;

        loop {
            let mut line = String::new();
            match proc.stdout.read_line(&mut line) {
                Ok(0) => {
                    return Err((
                        format!("ИИ-модуль неожиданно завершился.{INSTALL_HINT}"),
                        true,
                    ))
                }
                Ok(_) => {
                    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                        continue; // не наш вывод — пропускаем
                    };
                    if v.get("id").and_then(|x| x.as_u64()) != Some(id) {
                        continue;
                    }
                    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
                        let mut msg = err.to_string();
                        if msg.contains("No module named") {
                            msg.push_str(INSTALL_HINT);
                        }
                        return Err((msg, false));
                    }
                    return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                }
                Err(e) => return Err((format!("ИИ-модуль: {e}"), true)),
            }
        }
    }
}
