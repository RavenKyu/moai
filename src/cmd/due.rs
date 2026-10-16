//! 기한이 우선순위를 올린다(moai-dr8a.1q4, 2026-10-16 사용자 결정).
//!
//! **자동의 자리는 이 동사 하나다.** 읽는 길(`status`·`ready`·훅)은 줄을 고치지 않는다 — 읽기가 쓰기 시작하면
//! 보드를 한 번 본 것이 diff 를 남기고, 훅이 트래커를 쓰면 세션마다 서로의 쓰기와 겨룬다. 그래서 `status` 는 뒤처진
//! 줄을 알림으로 비추고, 사람·감독이 이것을 친다.
//!
//! **올리기만 한다.** 판정은 `report::due_raises` 의 순수 함수고, 여기는 그 답을 `store::with_write` 한 번 안에서
//! 적는다 — 우선순위를 바꾼 줄마다 같은 쓰기에서 `Due-priority:` 노트를 단다(`mv --take` 의 `Taken-over:` 와 같은
//! 꼴). 원래 값을 담는 필드는 없다: 그 값은 노트에 남는다.

use super::{Ctx, Fail, R};
use crate::cli::DueCmd;
use crate::model::{self, JournalEntry};
use crate::style::{self, paint};

/// 올린 줄 하나 — 판정이 빌린 줄을 놓고 쓰기로 넘어가려고 **제 것으로** 든다.
#[derive(serde::Serialize)]
struct Raised {
    id: String,
    title: String,
    /// 올리기 전의 우선순위.
    was: u8,
    /// 오른 우선순위.
    to: u8,
    /// 읽는 사람의 달로 잰 남은 날. 지난 것은 음수다.
    days_left: i64,
    /// 따른 기한.
    due_on: String,
    /// 그 기한을 든 조상 — 제 기한이면 키가 없다.
    #[serde(skip_serializing_if = "Option::is_none")]
    due_from: Option<String>,
}

impl Raised {
    fn of(r: &crate::report::Raise<'_>) -> Raised {
        Raised {
            id: r.issue.id.clone(),
            title: r.issue.title.clone(),
            was: r.issue.priority(),
            to: r.to,
            days_left: r.left,
            due_on: r.due.to_string(),
            due_from: r.from.map(|f| f.id.clone()),
        }
    }

    /// 노트 한 줄 — `Due-priority: p2 → p0 (1 days left, due 2026-10-17)`, 조상의 기한이면 끝에 `from <id>`.
    /// **꼴을 바꾸지 않는다** — 사람이 정한 꼴이고, 읽는 쪽이 줄 머리로 찾는다. 낱말은 화면 말을 안 따른다: 노트는
    /// 파일에 남는 글이다.
    fn note(&self) -> String {
        let from = self.due_from.as_deref().map(|f| format!(" from {}", crate::text::one_line(f))).unwrap_or_default();
        format!("Due-priority: p{} → p{} ({} days left, due {}{from})", self.was, self.to, self.days_left, self.due_on)
    }
}

pub fn run(ctx: &Ctx, cmd: DueCmd) -> R<Vec<String>> {
    match cmd {
        DueCmd::Apply { dry_run } => apply(ctx, dry_run),
    }
}

/// 판정을 제 것으로 든다 — 판정은 빌린 줄을 내고, 쓰기는 그 줄을 고친다. 올리는 것은 **산 줄뿐이고** 아카이브의 줄은
/// 계획 밖을 잴 문맥(미룬 에픽)으로만 읽는다([`crate::report::due_raises`]).
fn judged(
    issues: &[model::Issue],
    archived: &[model::Issue],
    unread: &[crate::store::LoadError],
    zone: &crate::tz::Zone,
    rule: &crate::config::DueRule,
    now: &str,
) -> Vec<Raised> {
    let context = super::in_context(issues, archived, unread);
    crate::report::due_raises(issues, &context, now, zone, rule).iter().map(Raised::of).collect()
}

fn apply(ctx: &Ctx, dry_run: bool) -> R<Vec<String>> {
    let repo = super::open_repo(ctx)?;
    let lang = ctx.lang();
    // **시간대는 락 밖에서 한 번 푼다** — 처음 부르면 사용자 설정을 연다(`mv` 가 `lang` 을 미리 푸는 것과 같은 까닭).
    let zone = ctx.zone();
    let rule = repo.config.due;
    let raised: Vec<Raised> = if !rule.auto {
        // **꺼진 규칙은 아무것도 안 바꾼다** — 앞으로의 변경만 멈추고, 이미 올린 값은 그대로 둔다(되돌리지 않는다).
        Vec::new()
    } else if dry_run {
        // 미리 보기는 읽기다 — 누구인지 안 묻고 락도 안 잡는다.
        let load = repo.read()?;
        let archived = match load.issues.iter().any(|i| i.due_on.is_some()) {
            true => crate::archive::read(&repo.root).unwrap_or_default().issues,
            false => Vec::new(),
        };
        judged(&load.issues, &archived, &load.errors, zone, &rule, &model::now())
    } else {
        // **누구인지는 락 밖에서 묻는다**(`mv` 와 한 자) — `git` 을 두 번 띄운다. 펴는 자리는 바꿀 것이 있을 때뿐이다:
        // 바꿀 것이 없으면 아무것도 안 쓰므로, 신원 없는 기계에서도 0 으로 끝난다.
        let who = model::actor(ctx.user.as_deref(), &repo.root).map_err(|e| Fail::no_actor(&e, lang));
        repo.with_write_after(
            || lang,
            |issues, unread, cfg, _| {
                // **시각은 락을 쥔 뒤에 뜬다** — `mv` 와 같은 까닭이다. 남은 날도 이 시각으로 잰다.
                let at = model::now();
                let archived = match issues.iter().any(|i| i.due_on.is_some()) {
                    true => crate::archive::read(&repo.root)?.issues,
                    false => Vec::new(),
                };
                let raised = judged(issues, &archived, unread, zone, &cfg.due, &at);
                if raised.is_empty() {
                    return Ok((Vec::new(), raised));
                }
                let by = who?;
                let mut entries = Vec::new();
                for r in &raised {
                    let Some(i) = issues.iter_mut().find(|i| i.id == r.id) else { continue };
                    i.priority = Some(r.to);
                    i.updated_at = at.clone();
                    i.normalize();
                    entries.push(JournalEntry::note(&i.id, &r.note(), &at, &by));
                }
                Ok((entries, raised))
            },
            |_| Ok(()),
        )?
    };

    if ctx.json {
        #[derive(serde::Serialize)]
        struct Out<'a> {
            /// `auto` 또는 `off` — 꺼졌으면 `raised` 는 늘 비었다.
            due_priority: &'static str,
            dry_run: bool,
            raised: &'a [Raised],
        }
        return super::json_line(&Out { due_priority: rule.mode(), dry_run, raised: &raised });
    }
    if !rule.auto {
        return Ok(vec![paint(style::DIM, crate::i18n::say(lang, "due.off")).to_string()]);
    }
    if raised.is_empty() {
        return Ok(vec![paint(style::DIM, crate::i18n::say(lang, "due.none")).to_string()]);
    }
    let mut out: Vec<String> = raised
        .iter()
        .map(|r| {
            let from = match &r.due_from {
                Some(f) => format!(
                    "  {}",
                    crate::i18n::fill(crate::i18n::say(lang, "due.from"), &[("id", &crate::text::one_line(f))])
                ),
                None => String::new(),
            };
            format!(
                "{}  {}   {}  {}{}   {}",
                paint(style::ID, &r.id),
                paint(style::PLAIN, &format!("p{} → p{}", r.was, r.to)),
                paint(style::DIM, &r.due_on),
                paint(if r.days_left < 0 { style::WARN } else { style::DIM }, &crate::view::due_tail(r.days_left, lang)),
                paint(style::DIM, &from),
                paint(style::DIM, &crate::text::one_line(&r.title)),
            )
        })
        .collect();
    if dry_run {
        out.push(paint(style::DIM, crate::i18n::say(lang, "due.dry_run")).to_string());
    }
    Ok(out)
}
