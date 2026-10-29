# Documentation Style

The writing and layout rules for every document in this repository. Read it before you add, rewrite or translate a document. The Chinese counterpart is `docs/doc-style.md`. **This English file is normative.** When the two disagree, this file wins for a wording question and the Chinese file is corrected in the same change. §4.3 decides which twin wins for a question of fact.

A rule is one of two levels. **MUST** means a violation is a review finding. **SHOULD** means the default, with a reason required for every exception. The reference sources are named in §1.3 and listed in §15.

---

## 1. Scope

### 1.1 What this guide governs

1.1 MUST apply this guide to every Markdown document here: the root files, all of `docs/`, and `release_doc/` and `release_doc_dev/` with their `*-en.md` twins.

1.2 MUST follow AGENTS.md for the workflow a document change goes through: the mandatory workflow, the review budget and triage, and the independent reviewer.

### 1.2 Audience

1.3 MUST write for three readers: a person who uses the app, a developer who changes it, and a model session that reviews the change. Write so that each of them reads a sentence once and gets one meaning.

1.4 MUST NOT write marketing copy. Do not use a word whose only job is praise, such as robust, powerful, seamless, comprehensive, best-in-class or cutting-edge.

### 1.3 Relation to the three reference sources

1.5 MUST adapt the principles of the three sources listed in §15, and MUST NOT claim compliance with any of them. This repository does not implement ASD-STE100 and has no approved-word list. Its word list is the glossary in §8.4.

1.6 MUST take the English rules from ASD-STE100. In its Issue 9 (2025-01-15) the standard has 53 writing rules in 9 sections and a dictionary of about 900 approved words. The sections `§6.2`, `§6.4`, `§6.7`, `§7.1` to `§7.3` and `§12.1` to `§12.2` are adapted from its rules.

1.7 MUST take the Chinese sentence ceilings in §6.5, the 的 chain limit in §7.15, and the nominalization ban and the
four modals in §7.7 and §7.10 from STC-0.1, which rebuilds those principles for Chinese.

1.8 MUST take the file naming in §3, the punctuation conventions used by the Chinese file and the document-system layout in §2 from ruanyf/document-style-guide.

### 1.4 What this guide does not govern

1.9 MUST read AGENTS.md *Editing and localization* for code comments. Comments are English only, and they appear only where the code cannot explain itself. That section wins over this guide for a comment.

1.10 MUST read `docs/design-style-en.md` for every token value: a color, a type size, a radius, a spacing value or a duration. This guide says how to write a document; it does not say what the UI looks like.

1.11 MUST read `docs/developer-notes-en.md` for subsystem behaviour. A document that restates it instead of linking to it has two owners for one fact.

1.12 MUST read `ARCHITECTURE.md` for the source tree. A document that draws its own copy of the tree drifts from the real one.

1.13 MUST NOT write the commit message style, the reviewer settings or the test tiers here beyond the cross-references above. AGENTS.md owns each of them.

1.14 MUST record the release process in `docs/releasing-en.md`. Do not add a second release procedure to a plan document.

1.15 SHOULD name the owning document when a sentence about another subsystem is unavoidable. The reader then knows where the fact is maintained.

1.16 SHOULD keep a document under 400 lines. A longer document gets a section of its own or a link to the owner, because a reader stops at the first screen it cannot scan.

## 2. Document types and location

2.1 MUST keep a document in the location that §2.2 gives for its type. A new document MUST state its type in its first paragraph.

### 2.2 Types, locations and required sections

| Type | File | Required sections, in this order | Status header |
| --- | --- | --- | --- |
| README | `README.md` and `README-ZH.md` | What the app is; feature list with a link per feature; install; permissions; link to the other language | None |
| Architecture map | `ARCHITECTURE.md` | Purpose; Tree (file by file); ownership and layering rules; threading rules; where each kind of change belongs | None; age is visible in the tree itself |
| Agent instructions | `AGENTS.md` | Build, run and verification; testing tiers; workflow; review; invariants | None; it describes the present |
| Developer notes | `docs/developer-notes.md` and `docs/developer-notes-en.md` | One `##` section per subsystem; each section states the contract, the measured fact and the command that reproduces it | None |
| Design style | `docs/design-style.md` and `docs/design-style-en.md` | Scope; principles; one numbered section per dimension (color, type, spacing, radius, elevation, motion, language); deviations | None; the normative document is current by definition |
| UI refresh plan | `docs/ui-refresh-plan.md` and `docs/ui-refresh-plan-en.md` | Ground rules; measured baseline; phases; the assertion each phase lands | Required: a `Status:` line naming the current phase |
| Releasing | `docs/releasing.md` and `docs/releasing-en.md` | Production pipeline; development channel; signing; icon generation; what is not removed on uninstall | None |
| Plan or audit document | `docs/*-plan.md` and `docs/*-audit.md` | `状态：` or `Status:` as the first list item; scope; findings or phases; decisions and their reasons; open questions | Required, and MUST carry a date |
| Review backlog | `docs/review-backlog.md` | Header paragraph stating what an entry is; open table; closed entries; corrections | None; each entry carries its own date |
| Release notes | `release_doc/<version>.md` | One locale marker line; a version heading; what is new; notes | Version heading only; the file name is the version |

2.3 MUST put a status header on every plan, audit and UI refresh document. The header states what is true now: `Status: design agreed, code not started`, `状态：代码已实施，A2 未运行` or an equivalent sentence that a reader can check against the repository.

2.4 MUST keep a status header true. A header that says `已评审通过` or `implemented` after a later change invalidated it is a defect, not a stale detail.

2.5 MUST put release notes in `release_doc/<version>.md`, where `<version>` is the `Cargo.toml` version. A development build reads `release_doc_dev/<version>.md` through `scripts/dev-restart.sh`.

2.6 MUST write each release note line as a claim a user can check in the running app. Do not repeat a rule from another document.

2.7 MUST decide whether a release note is frozen by its own channel and version, not by its file name. The two
channels are independent feeds: `scripts/release.sh` publishes `release_doc/<version>.md` to the stable feed, and
`scripts/release-dev.sh` publishes `release_doc_dev/<version>.md` to the dev feed. They may carry the same version
number, so a version reaching one feed proves nothing about the other: dev may have shipped 0.2.6 while only the
`release_doc/0.2.6.md` draft is being written, and the reverse is just as possible. A note is frozen only when
THAT file has been published with its own build on its own feed.

2.8 MUST NOT change a frozen release note, and MUST check the specific file before editing any other one. A
frozen note is the record of what a published version told its users: it has been shown inside the app, quoted on
a release page, and read by people who never update the file. A later correction — a wrong claim, a missing
bullet, a reworded sentence — does not make the old note wrong; it makes it history, and it belongs in the next
version's note. An unpublished draft is editable, in either channel: correcting an unshipped note and writing the
note for the version in preparation are how a release is made. Before editing, establish which file it is — the
channel, the version, and whether that version was published in that channel — and say so in the commit message.

## 3. File names and layout

3.1 MUST name a *new* document with lowercase ASCII letters, digits and hyphens. Do not use a space, a full-width
character or an underscore. The names §2 fixes are the exception: `README.md`, `README-ZH.md`, `AGENTS.md`,
`ARCHITECTURE.md` and `release_doc/<version>.md` keep the spelling the repository and its tools require.

3.2 MUST use the `-en` suffix for the English twin of a Chinese document: `docs/developer-notes.md` is Chinese and `docs/developer-notes-en.md` is English. This is the convention the repository already follows.

3.3 MUST put every untranslated Chinese document under `docs/`. A root file is either an instruction file (`AGENTS.md`), a map (`ARCHITECTURE.md`), a README or a licence.

3.4 MUST give a twin pair the same stem. `${stem}.md` and `${stem}-en.md` then pair by construction, and a script
finds a missing twin from the file names. A root file is a twin only when its stem says so (`README.md` and
`README-ZH.md`); a release note is one file with the three locale blocks inside it, not a pair.

## 4. Twin rules

4.1 MUST change both twins in the same commit. A change to one file alone leaves the other claiming the old behaviour.

4.2 MUST NOT let a twin introduce a claim that the other lacks. A sentence, a number, a command or a warning that exists in one language and not the other is a divergence, even when the other file is still readable.

4.3 MUST settle a disagreement in favour of the twin that matches the code and the measured behaviour, for a question of fact, and MUST correct the other twin in the same change. The one exception is the design style, where AGENTS.md *UI design system* makes the document win and the code the bug.

4.4 MUST keep the rule numbers identical in both files. A number names the same rule in each twin.

4.5 SHOULD keep table rows, list items and headings in the same order. A reordered section makes the bilingual diff unreadable.

4.6 MUST link the twins: each file names the other in its first paragraph and says which one is normative. The Chinese file MUST say that `docs/doc-style-en.md` is normative.

4.7 MUST NOT translate a user-visible string in a twin. Quote the string exactly as the app renders it in that locale, and name the locale.

## 5. Headings

5.1 MUST use one `#` heading per file, and it is the document title.

5.2 MUST use at most three levels in a document under `docs/`: `#`, `##` and `###`. A document longer than 300 lines may use a fourth.

5.3 MUST NOT skip a level. A `###` heading under a `#` heading with no `##` between them is a defect.

5.4 MUST NOT leave a sub-heading alone. A heading with no peer at its level is a list item with an extra level of indentation.

5.5 MUST NOT repeat the parent's name in a sub-heading. Write `### Storage` under `## Clipboard history`.

5.6 MUST NOT end a heading with a period, a colon, a comma or a semicolon.

5.7 MUST use one capitalization style for headings in a file: sentence case for English, or title case for English. Do not mix the two.

5.8 SHOULD number a heading when the document is cited by section number, as the design style is. The number and the title are one string: `## 3. Color`.

5.9 MUST make a heading a noun phrase or a question. Do not write a sentence as a heading.

## 6. Sentences and paragraphs

6.1 MUST make one sentence carry one instruction or one fact. Two instructions mean two sentences or a numbered list.

6.2 MUST keep an English sentence in a procedure to 20 words or fewer. The counting method is 6.4.

6.3 MUST keep an English descriptive sentence to 25 words or fewer.

6.4 MUST count an inline code span, an identifier, a number with its unit, a quoted string and a fenced block each as one word. Count a Markdown link by its text, not its URL. Hyphenated compounds count as one word.

6.5 MUST keep a Chinese sentence in a procedure to 30 characters or fewer, and a Chinese descriptive sentence to 40 characters or fewer. Count each Chinese character as 1, each code span, number or quoted string as 1, and do not count punctuation.

6.6 MUST use at most 2 commas in a Chinese sentence. A 、 in a list does not count. More than 2 commas means the sentence is split or written as a list.

6.7 MUST put the condition first and the action second: `If the log window is unreadable, report build timestamp: unverified.` and `如果日志窗口不可读，报告 build timestamp: unverified。`

6.8 MUST start a new sentence when the subject changes. Do not let a comma carry a new subject, as in `服务器重启后，打开浏览器，显示登录页。`.

6.9 MUST use one topic per paragraph, with at most 6 sentences. A paragraph that needs a seventh sentence needs a heading or a list.

6.10 MUST put the conclusion in the first sentence of a paragraph. Reasons and detail follow it.

6.11 MUST use a list or a table for 3 or more parallel items.

6.12 MUST write a step as a numbered item with one action. Number a procedure from 1, and give the expected result on its own line after the step.

## 7. Voice and modality

7.1 MUST use the active voice and name the agent. Write `The app writes the launch record.`

7.2 SHOULD use the passive voice in descriptive text only when the agent is unknown or irrelevant.

7.3 MUST use the simple present, the simple past or the simple future in English. Do not use a compound auxiliary construction where a simple tense says the same thing.

7.4 MUST write a procedure step as an imperative: `Run scripts/gate.sh`, not `You should run scripts/gate.sh`.

7.5 MUST NOT use a contraction in English text: write `do not`, `cannot` and `it is`.

7.6 MUST NOT nominalize an action in English. Write `delete the entry`, not `perform the deletion of the entry`.

7.7 MUST NOT write `进行`, `予以`, `加以`, `实施` or `实现` before a verb in Chinese. Write the verb: `删除异常数据`, not `对异常数据予以删除`.

7.8 MUST keep an English modal to one of four, each with one meaning:

| Modal | Meaning | Do not write |
| --- | --- | --- |
| must | Required. A failure follows without it | should, need to, required to, have to |
| do not | Forbidden | must not, never (outside a WARNING), avoid |
| can | Allowed. The reader may also skip it | may, might, could |
| recommended | A choice with a reason | should, ideally, best practice, 最好, 尽量 |

7.9 MUST NOT write `should` in normative text. A human or a model reads it as optional, so such a requirement is treated as a suggestion.

7.10 MUST keep a Chinese modal to one of four: `必须` (required), `不要` (forbidden), `可以` (allowed), `建议` (recommended, with the reason). Do not write `应`, `应当`, `需`, `务必`, `不得`, `最好` or `宜`.

7.11 MUST NOT use a double negation. Write `必须检查电源`, not `不能不检查电源`.

7.12 MUST NOT use a vague word where a number, a name or a complete list fits: `适当`, `相关`, `若干`, `某些`, `尽量`, `及时`, `基本上`, or a trailing `等`. Write `60 个目录`, not `若干目录`.

7.13 MUST NOT use a vague catch-all verb such as `处理`, `搞` or `弄`. Name the action: `修复这个报错`.

7.14 MUST use `必须` rather than `应` for a requirement, because `应` is read with the same optionality the English `should` carries.

7.15 SHOULD keep at most 2 `的` in one Chinese noun phrase. A third `的` means the modifiers are stacked too deep:
write 用户账户安全设置的默认值, not 用户的账户的安全设置的默认值.

7.16 MUST NOT join two sentences with a semicolon in normative text. Write two sentences, or a list. A
semicolon inside a table row, a code span or a quoted command is data, not prose, and stays.

## 8. Terminology

8.1 MUST use one term for one thing and one thing for one term, in both languages, and across the whole file.

8.2 MUST use the glossary term in §8.4 for each repository concept. A document that needs a term which is not in the table adds it to the table first, in both twins, with its one meaning and its banned substitutes.

8.3 MUST NOT use a banned substitute from the table, even where ordinary English allows it.

### 8.4 Glossary

| Term (中文) | Its one meaning in this repository | Do not write |
| --- | --- | --- |
| overlay (浮窗) | The floating panel the switcher draws over the desktop | popup, HUD, window, 浮层 |
| switcher (切换器) | The Summon → cycle → raise surface, and the feature that owns it | app switcher, cmd+tab tool, window list |
| card (卡片) | One selectable window inside the overlay: an icon-only card or a thumbnail card | tile, item, cell |
| clipboard picker (剪贴板选择器) | The panel that lists clipboard history entries | clipboard window, paste panel, 剪贴板窗口 |
| clipboard detail panel (剪贴板详情面板) | The second panel that shows one entry in full | preview window, detail view |
| settings window (设置窗口) | The window with the sidebar and its pages | preferences, options dialog |
| onboarding guide (引导) | The first-run window that asks for permissions and shortcuts | welcome wizard, tutorial |
| dev bundle (开发构建) | `dist/Oh-My-Tab-Dev.app`, the signed development build | debug build, test app |
| launchd job (launchd 任务) | A job registered with launchd for the app or its Finder extension | daemon, service, agent |
| worktree (worktree) | A git worktree of this repository; one build/test/launch transaction runs at a time across all of them | checkout, copy, clone |
| gate (门禁) | `scripts/gate.sh` and its tiers, each printing one PASSED, FAILED or NOT RUN line | CI run, test suite, check |
| M1 (M1 格) | Code-decided and non-disturbing: the headless offline tier, every `scripts/*-selftest.sh`, and the smoke runners that start no app, place no window on screen, grab no screen, inject no input and restart no instance. This is the daily gate | smoke tier, unit tests, daily gate |
| M2 (M2 格) | Code-decided and disturbing: the remaining smoke runners and the `scripts/e2e/` scenarios. Selected only by `gate.sh --full` | integration tests, UI tests, release gate |
| A1 (A1 格) | Agent-decided and non-disturbing: an agent reads state or an **existing** screenshot file. Never gate evidence | manual review |
| A2 (A2 格) | Agent-decided and disturbing: the agent must watch live (real-time UI review, live triage). Never gate evidence | manual review |
| disturbance marker (干扰标记) | A declaration in a scenario's or runner's header: focus, screen, preferences, or replaces-instance. Each needs its own authorization; none implies another | `E2E_*` / `SMOKE_CELL` marker |
| smoke runner (冒烟运行器) | One `#[ignore]` test carrying exactly one `SMOKE_CELL=M1\|M2` declaration, run and classified by its `smoke-result:` line | smoke test, GUI test |
| e2e scenario (e2e 场景) | One `scripts/e2e/*.sh` script run through `scripts/e2e/run-all.sh`, always M2 | e2e test, M2 test |
| disturbance switch (干扰闸门) | The `--include-focus`, `--include-screen` or `--include-prefs` consent for one declared disturbance | flag, opt-in |
| reviewer session (审查会话) | The one `scripts/agent-review.sh` session that belongs to a task | review bot, ChatGPT session |
| review round (审查轮次) | One complete round of the reviewer that ends in a verdict | review, iteration, pass |
| review backlog (审查积压) | `docs/review-backlog.md`, and the deferred findings recorded there | TODO list, known issues |
| Accessibility permission (辅助功能权限) | The TCC grant for the event tap and AX operations | AX access, special access |
| Screen Recording permission (屏幕录制权限) | The TCC grant that thumbnail capture needs, and without which the app falls back to icons | capture permission, screen access |
| User-visible wording (用户可见文案) | Text the app draws, which lives in `t()`, `tf()` or `t_count()` | UI text, label, strings |

## 9. Code, data and UI strings

9.1 MUST set a path, a command, a configuration key, an identifier, a selector, a file name and a state name in inline code: `~/.config/oh-my-tab/config.toml`, `scripts/gate.sh`, `needs_in_place_control_sync`, `SectionHeader`.

9.2 MUST show a transcript inside a fenced block, with the command that produced it immediately before the block. A block without its command is an unverifiable quote.

9.3 MUST quote a user-visible string exactly as the app draws it. Do not paraphrase it, translate it out of its locale, or correct its capitalization. Name the locale when the string differs between locales.

9.4 MUST NOT print a raw translation key, a config key the app never shows, an enum name, a hex value, a VID or PID, or an internal preset id in a document addressed to users. These belong in a developer document, inside inline code, next to the surface that shows the real string.

9.5 MUST write a developer log line in English, including in a Chinese document. Quote the line exactly and name the log file it comes from.

9.6 MUST set a table of measured values as a table, and name the command that produced each column in the table's caption or the sentence above it.

## 10. Numbers, units, dates and times

10.1 MUST reproduce every quantitative claim with one named command. The command belongs in the same section as the number.

10.2 MUST treat a number as void when the instrument behind it changed. Correct every document that quotes it in the same change. This is the reviewer rule AGENTS.md *What the reviewer is asked to weigh here* states.

10.3 MUST write a quantity as digits with its unit. Do not spell out a number.

10.4 MUST put a space between a number and its unit (9.1 GB, 750 ms, 60 pt), except before % and ° (80%, 15°).

10.5 MUST write a range with the unit on both ends: `10 V to 20 V`, `10 V 至 20 V`, `60 pt 至 76 pt`. Do not write the unit once at the end.

10.6 MUST write a date as ISO 8601: 2026-10-24.

10.7 MUST name the time zone whenever a document mentions more than one zone, as in `2026-10-24 09:30 (Asia/Shanghai)`.

10.8 MUST write an increase or a decrease with `increased to`, `increased by`, `decreased to` or `decreased by`. Do not write `reduced by 3 times` or `减少 3 倍`.

## 11. Tables, lists, links, cross-references and diagrams

11.1 MUST use a table for 3 or more parallel items that carry the same attributes, and a list for 3 or more plain items.

11.2 MUST link a repository file by its repository-relative path, in inline code, with an optional line anchor: `docs/developer-notes-en.md:596`. Do not hide the path in a relative link.

11.3 MUST NOT write `see above`, `as stated earlier`, `同上` or `如上`. Write the section number and the path instead.

11.4 MUST make link text say what the target is. Write `[the gate script](scripts/gate.sh)`. Do not write `[here]`.

11.5 MUST name the page in an external link. Do not link a search result or a redirect.

11.6 MUST draw a flow with a branch as a Mermaid block or an ASCII block. Do not describe a branch in prose when the reader must hold three outcomes at once.

11.7 MUST NOT put more than 7 columns in a table. A wider table is two tables or a list.

## 12. Warnings and destructive operations

12.1 MUST map the two levels exactly:

| Level | Use it for | Mark it |
| --- | --- | --- |
| WARNING / 警告 | A risk of personal injury, or of exposing private data | `WARNING:` / `警告：` |
| CAUTION / 注意 | A risk of data loss, a broken install, or an unusable app | `CAUTION:` / `注意：` |

12.2 MUST put the instruction in the first sentence and the exact consequence in the second: `CAUTION: Delete the keychain item only after the history file is gone. Deleting it first leaves data that can never be decrypted again.`

12.3 MUST place a warning before the step it applies to, outside the step, and MUST NOT place it after the step.

12.4 MUST give one risk per warning. Two risks are two warnings.

12.5 MUST document a destructive command with what it destroys and how to recover. This covers `git reset`, `rm`, a force push, `scripts/agent-review.sh --finish` and a deleted keychain item. The recovery sentence MUST say what cannot be recovered.

12.6 MUST NOT put a warning in a table cell. A warning needs its own paragraph.

## 13. Review and maintenance

13.1 MUST send a documentation change through the same independent review as a code change, under the same budget and triage rules in AGENTS.md.

13.2 MUST treat a stale document as a defect class, not as polish. A document that describes behaviour the app no longer has is an in-scope finding even when its severity label is low.

13.3 MUST correct the document when the code is the fact. The design style is the one exception, where AGENTS.md makes the document win and the code the bug.

13.4 MUST record a deferred documentation finding in `docs/review-backlog.md` with its round, its severity, why it is deferred and the trigger that brings it back.

13.5 MUST delete a backlog entry when its work lands, and say so in the commit message or the release notes.

13.6 MUST update a document in the same change as the code it describes. A follow-up commit that fixes the document later is the defect §13.2 names.

13.7 MUST re-read §2.4 before a release: every status header is checked against the tree, and a header that is no longer true is corrected in that release.

13.8 SHOULD add a numbered rule here rather than a paragraph of guidance. A review cites a rule number, never a paragraph.

## 14. Pre-publication self-check

Check every line before a document is published. Each line cites the rule it checks.

- [ ] The file is in the location for its type, and it declares its type (§2.1, §2.2).
- [ ] A plan, audit or refresh document has a status header, and the header is true now (§2.3, §2.4).
- [ ] No release note published on its own channel was changed; every edited draft is one that channel has not published (§2.7, §2.8).
- [ ] The file name is lowercase ASCII with no space, and the twin stem matches (§3.1, §3.4).
- [ ] Both twins changed in this commit, with the same rule numbers (§4.1, §4.4).
- [ ] Each twin names the other and says which one is normative (§4.6).
- [ ] No heading skips a level, stands alone or repeats its parent (§5.3, §5.4, §5.5).
- [ ] Every English sentence is within 20 words as a step or 25 as description, counted by §6.4 (§6.2, §6.3).
- [ ] No semicolon joins two sentences of prose; a table cell or a quoted command keeps its own (§7.16).
- [ ] Every Chinese sentence is within 30 or 40 characters, with at most 2 commas (§6.5, §6.6).
- [ ] Every paragraph has its conclusion first and at most 6 sentences (§6.9, §6.10).
- [ ] The text uses no `should`, no contraction and no nominalization in either language (§7.5, §7.7, §7.9).
- [ ] Every Chinese modal is one of the four (§7.10).
- [ ] No vague word and no double negation remains (§7.11, §7.12).
- [ ] Every repository term is the one in the glossary, and a new term was added to the table first (§8.2, §8.4).
- [ ] Every path, command, key and identifier is inline code, and every transcript has its command (§9.1, §9.2).
- [ ] Every user-visible string is quoted exactly, and no raw translation key appears (§9.3, §9.4).
- [ ] Every number has one named command that reproduces it (§10.1).
- [ ] Units have a space, ranges repeat the unit, dates are ISO 8601 (§10.4, §10.5, §10.6).
- [ ] No `see above`, no bare `here` link, and every file link carries its path (§11.2, §11.3, §11.4).
- [ ] Every warning has the instruction first, the exact consequence second, and sits before its step (§12.2, §12.3).
- [ ] Every destructive command states what it destroys and how to recover (§12.5).
- [ ] Every deferred finding is in the backlog with its revisit trigger (§13.4).

## 15. Sources

15.1 MUST adapt the following three sources, and MUST NOT claim compliance with any of them. ASD-STE100 is a trademark of ASD, and STC is an unofficial draft by JIC School that has no affiliation with ASD.

| Source | What this guide takes | URL |
| --- | --- | --- |
| ASD-STE100 Simplified Technical English, Issue 9, 2025-01-15. 53 writing rules in 9 sections and a dictionary of about 900 approved words | The 20-word and 25-word sentence ceilings, one instruction per sentence, the imperative procedure, the condition first, the active voice, one topic and 6 sentences per paragraph, the instruction-then-consequence warning, the semicolon rule of §7.16, and the counting method of §6.4 | <https://www.asd-ste100.org/> |
| ruanyf/document-style-guide (public domain) | Lowercase hyphenated file names, the document-system layout, the Chinese punctuation conventions and the numeric-range convention | <https://github.com/ruanyf/document-style-guide> |
| JICio/simplified-technical-chinese, STC-0.1 (unofficial draft, CC BY 4.0) | The 30-character and 40-character ceilings, 2 commas, the 的 chain limit, the nominalization ban, the four Chinese modals, the instruction-first safety note and the conclusion-first paragraph | <https://github.com/JICio/simplified-technical-chinese> |

15.2 MUST NOT claim ASD-STE100 compliance in any document, README or release note. Write `adapted from the principles of ASD-STE100` and name the rule that was adapted.

15.3 SHOULD re-read a source before quoting it. The STC sentence ceilings are estimates, not measured values. A source's numbering does not move to this guide: `§6.2` is this guide's number.
