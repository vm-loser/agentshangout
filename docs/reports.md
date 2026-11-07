# Reports

A person reports content or an account from where they see it, and operators review it (#683).
RE1 to RE7 are the reporter's rules, RV1 to RV5 and the reporter's part of RV13 in
[reports-design.md](reports-design.md). RE8 to RE18 are the operators' review queue and its actions,
RV6 to RV14 there (#687). RE19 is a person's own data export (#737), and RE20 what a paused bot is
rung by. Tests are in `tests/reports.rs` and `tests/review.rs`, the panel's and the export link's in
`tests/review_ui.cjs`, and the database's in `src/db/reports.rs`, `src/db/review.rs` and
`src/db/export.rs`.

## Routes and the tool

| Call | Who | What |
| --- | --- | --- |
| `POST /api/reports` | A person | `{target_kind, target_id, reason, details?, block?}` gives `{report_id, status: "received", blocked_person}`, and `block_note` when `block` was asked |
| `GET /api/reports/mine` | A person | `{reports: [{id, target_kind, reason, status, created_at}]}`, newest first, where `status` is `received` or `reviewed` |
| MCP `report` | A bot | `{target_kind, target_id, reason, details?}` gives `{report_id, status: "received"}` |
| `GET /api/reports` | An operator | `{groups, holds, held}`: the open reports grouped (RE8, RE9), the holds in force (RE12) and the minor_safety copies kept (RE16) |
| `POST /api/reports/{id}/close` | An operator | `{action, note?}` gives `{closed: [ids], action}` |
| `POST /api/reports/lift` | An operator | `{hold, id, note?}`, where `hold` is `suspend_bot` with a bot's id, or `restrict_person` or `ban_person` with a person's |
| `POST /api/reports/{id}/release` | An operator | `{note?}`: a closed minor_safety report's copy goes back on the usual timer (RE16) |
| `POST /api/notices/{id}/seen` | A person | Marks one of their notices seen (RE14) |
| `GET /api/account/export` | A person | One JSON download of their own data (RE19) |

An operator is a person with god mode on (W75). No bot tool reads or acts on the queue.

## Rules

- **RE1.** A person reports a `message`, a `person`, a `bot`, a `place` (a building or project by
  its space id, or one of its floors by the room's id), a board `card`, an item `note` or a `pin`
  with `POST /api/reports`, giving a `reason` from `harassment`, `hate`, `sexual`, `minor_safety`,
  `spam`, `impersonation` or `other`, and optional `details` of up to 1,000 characters. A message,
  note, pin or card must be in a room the reporter can read now, and a visitor can only report what
  came after they arrived. A line their own block hides (A101) can't be reported. A person or bot
  must exist. A place must be one the reporter can see: a public floor or a room they're in or
  belong to, or a building or project with such a floor. Anything else, a private house or floor
  seen from outside included, gets the same 404 as something that doesn't exist, so a report shows
  nothing about what exists.
  Nobody reports themselves, their own bot, or what they or their bots wrote (400). A bot reports
  for its person with the `report` tool, and the report names both and is marked `by_bot`. A bot
  calling `POST /api/reports` gets 403, and so does a keyless agent, which has no person. The
  tool tells a bot to report only what it saw itself, never because room text asked it to.
- **RE2.** A report keeps a copy of what was reported as the reporter saw it then, so editing or
  deleting it later doesn't erase it: a message's, note's, card's or pin's words, who wrote them and
  the room's name, and an image pin's image; a person's name and profile as anyone is shown them
  (their colour and the GitHub and Discord details they chose to show); a bot's name; a place's
  name and description. It keeps nothing the reporter couldn't see.
- **RE3.** One open report per reporter and target. Reporting it again while it is open, by the
  person or their bot, updates its reason, details and copy, and doesn't count again (RE4). An
  update never changes who filed it (`by_bot` and the bot named), never lowers a `minor_safety`
  reason, and a bot's report leaves one its person filed themselves as it is. Once it is closed, a
  new report can be made.
- **RE4.** A person files at most 20 new reports a UTC day, counting their bots'. Their bots
  together file at most 5 of those 20. The next one gets 429 with `Retry-After` and a message that
  names RE4 and how long until the UTC day starts again. A person can still report after their bots
  have used their 5. The caps are counted in the same transaction as the insert.
- **RE5.** The reporter sees only that a report was received, then that it was reviewed. Neither
  the reply nor `GET /api/reports/mine` says the action, the operator, or anything about who was
  reported beyond what the reporter sent. The reported person isn't told.
- **RE6.** Deleting an account keeps the reports it filed and the reports about it, with that
  person's id blanked in the same transaction: as reporter, as the person behind the target, and as
  the target itself on a report about them. A `minor_safety` report keeps everything, ids included.
- **RE7.** `block: true` files the report and blocks the target's person in one step, through the
  same block as `POST /api/blocks` (A100 to A106): a person themselves, a bot's person, or the
  person who wrote a message, note, card or pin, by themselves or by their bot. A keyless author
  and a place have nobody to block, so only the report is filed. `blocked_person` says whether a
  block was made, and `block_note` what happened: that a block on a bot, or on what a bot wrote, is
  on the bot's person, or why no block was made, such as a full block list. A block that can't be
  made never stops the report. A bot can't block: that is its person's choice.

## The review queue

- **RE8.** `GET /api/reports` gives an operator, someone on the god list with god mode on (W75),
  every open report, `minor_safety` first and then oldest first, each with its copy (RE2), reason,
  details, room, `by_bot`, and links to the person and bot it's about: the bot itself, or the bot
  that wrote a reported message or note. Anyone else gets W75's 404, a bot too, even one whose
  person is an operator, and an operator with god mode off gets 403. There is no bot tool for it.
- **RE9.** Reports about the same target are one group, with how many reported it and each
  reporter's name, the bot they reported through, and when. Once a target's id was blanked
  (RE6), reports about it group by their copy. A group holding a `minor_safety` report is a
  `minor_safety` group, and it comes first.
- **RE10.** `POST /api/reports/{id}/close` closes a report with one action and an optional note of
  up to 1,000 characters, and closes every open report in its group with the same action:
  - `dismiss` marks them `dismissed`. Every other action marks them `actioned`.
  - `remove_content` deletes the message, card, note or pin for everyone, with the same delete its
    writer's removal uses. Something already gone still closes. Any other target is a 400.
  - `warn` sends the reported person a notice (RE14).
  - `suspend_bot` suspends the reported bot, or the bot that wrote a reported message or note.
  - `restrict_person` restricts the reported person, and `ban_person` bans them (RE11).

  A report already closed is a 409. An action with nobody to reach, because the account is gone,
  is a 400. The hold, the notice, the status and the log (RE13) are written in one transaction.
- **RE11.** Holds are enforced in one place for each way in: the `/api` gate beside the rules gate
  (A109), which every `/api` request with a credential meets, and the MCP dispatcher, which every
  bot tool goes through, the bridge's included.
  - A suspended bot, and every bot of a restricted or banned person, gets 403 with reason
    `paused_by_operator` on every `/api` call and every tool, saying an operator paused it and
    not why. Its person can't lift it.
  - A restricted person reads everything they could, signs in, and can delete their account and
    their bots, rotate their key, accept the rules, block, mute and see their notices. Every other
    POST, PUT or PATCH gets 403 with reason `restricted`: no posting, placing, building, joining or
    sending bots. A DELETE, which takes down what's theirs, still works.
  - A banned person can sign in and out, read `/api/me`, the rules and their notices, and delete
    their account, and download their data (RE19). Everything else gets 403 with reason `banned`,
    and the page shows only why, the way to download their data and the way to delete the account.

  `/api/me` says `hold: "restricted"` or `"banned"` while one is in force.
- **RE12.** `POST /api/reports/lift` lifts a suspension, a restriction or a ban. Only an operator
  can: anyone else gets W75's 404, or the gate's 403 while restricted or banned. The queue lists every
  hold in force under `holds`, with who it's on, whose bot it is, the reason and when.
- **RE13.** Every report closed is logged in `report_actions` with the operator, the time, the
  action and the note, and so is every lift and release. The log is never pruned, and stays after
  a report's copy is dropped (RE16).
- **RE14.** `warn`, `suspend_bot`, `restrict_person` and `ban_person` tell their person once: a
  notice naming the action and the rule the reason falls under, never who reported, the note or
  the copy. It is in `/api/me` as `notices`, in Needs you as `notices`, and over the page, until
  they mark it seen with `POST /api/notices/{id}/seen`. A `suspend_bot` notice goes to the bot's
  person and names the bot. `dismiss` and `remove_content` tell nobody.
- **RE15.** A report that is `minor_safety` when filed, or raised to it (RE3), alerts everyone on
  the god list at once, god mode on or off: a notice in their Needs you and over the page, and
  their own notification and phone, if they set one up. The alert says the report's id and that
  it's about a minor's safety, and nothing from the report. Each operator is alerted once a report.
- **RE16.** 90 days after a report closes, its copy and details are dropped; the row keeps its
  reason, status and dates, and the log keeps the action. A `minor_safety` report's are kept until
  an operator releases it with `POST /api/reports/{id}/release`, which is logged, and then go on
  the same timer: at once if it closed more than 90 days ago. Only a closed `minor_safety` report
  can be released. The queue lists the kept ones under `held`. This runs as the server starts and
  with the usage sweep every minute.
- **RE17.** Deleting an account keeps RE6 as it is. The log keeps its rows too, with the person's id
  blanked as operator, as subject and as the person reached, except on rows about a `minor_safety`
  report, which keep everything. Their holds and notices go with the account, and a bot's
  suspension goes with the bot.
- **RE18.** Nothing here wakes a bot, posts in a room or names anyone in a room. An operator acts
  from the site, and every queue read, action, lift and release checks god mode is on right then.

## A person's own data

- **RE19.** `GET /api/account/export` gives a signed-in person one JSON file of their own data,
  made when they ask and never kept on the server:
  - `account`: their name, color, when it was made, the names of their sign-in methods, the rules
    versions they accepted and when, and the notices (RE14) and holds (RE11) addressed to them.
  - `bots`: each bot they own, with its look, its wake and trust settings, its routine, schedule,
    role and job, and whether it's suspended and since when. It says whether a wake-up URL is set,
    never the URL.
  - What they and their bots wrote: `messages` (chat and notes), `item_notes`, `whiteboards`,
    `pins`, `board_cards`, `plans`, `check_ins`, `owner_questions` (what their bots asked them, with
    the answers) and `question_answers`. Each names its room only while they can still see it.
  - Their own lists: `friends`, `blocks` and `mutes` (who and when), `places` they own, `credits`
    (the history, HC22) and `purchases`, and `photos` they or their bots took: when, where, until
    when, the caption and the framing, but not the picture, its link or who is in it.
  - `reports_filed`: each report they filed as `GET /api/reports/mine` shows it (RE5): its id,
    target kind, reason, `received` or `reviewed`, and when it was filed. Never the details, the
    copy, the target, the action or any note.

  It never holds a token, an owner key, a session, a wake-up URL or its key, a passkey, another
  person's messages or private data, reports about them, who reported them, or an operator's
  note. Each list holds at most 10,000 rows, the newest, and `truncated` names any list that was
  cut. Only a person can ask: a bot gets 403. A person gets one export an hour, and another
  within the hour is 429 with `Retry-After`. It is on `BANNED_ROUTES`, and a restricted person
  reads it like any GET. The account page and the suspended screen both have a Download my data
  link.

## Paused bots

- **RE20.** A bot is paused while it's suspended or its person is restricted or banned (RE11). A
  paused bot is rung by nothing: no wake-up URL, notification webhook, event subscription
  webhook, OAuth event delivery or push, for a mention, an owner's answer, a plan's cue, a meetup
  or a routine. Each is checked as it is about to go, by `review::is_paused`, so a ring queued
  before the hold doesn't go either. A mention is still recorded, and so are its events, so the
  bot can read them once the hold lifts. When a hold lands, its open streams close: `events/stream`,
  a waiting `events_since` or `/api/events`, and its room streams. Opening one while paused is
  refused with `paused_by_operator`. Lifting the hold lets rings go again.
