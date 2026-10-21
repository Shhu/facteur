// A stand-in for the Tauri engine, for screenshots of the UI in a plain browser.
// Loaded as an init script after data.js: it defines `window.__TAURI_INTERNALS__`, through which
// @tauri-apps/api sends every `invoke` (engine commands, and `plugin:…` calls for events, the
// window, paths, dialogs), and answers from `window.__FACTEUR_DEMO__`. Engine events are never
// emitted: the UI shows the data as first loaded.
;(() => {
  const demo = window.__FACTEUR_DEMO__
  if (!demo) throw new Error('mock.js: load data.js first')
  const { NOW, accounts, threads, held, logos } = demo

  // The UI language and per-device switches live in localStorage.
  try {
    localStorage.setItem('facteur:language', window.__FACTEUR_LANG__ || 'fr')
  } catch {}

  // ── Mailboxes ─────────────────────────────────────────────────────────────

  const ROLES = ['inbox', 'flagged', 'snoozed', 'drafts', 'sent', 'archive', 'junk', 'trash', 'screener', 'refused']
  const NAMES = { inbox: 'INBOX', flagged: 'Épinglés', snoozed: 'Plus tard', drafts: 'Drafts', sent: 'Sent', archive: 'Archive', junk: 'Junk', trash: 'Trash', screener: 'Nouveaux expéditeurs', refused: 'Refusés' }
  const FOLDERS = { 1: ['Factures', 'Voyages', 'Famille'], 2: ['Clients', 'Devis', 'Inspiration'], 3: ['Adhérents', 'Sorties', 'Comptabilité'] }
  /** Messages kept in each inbox (far more than the threads listed: older mail is read). */
  const INBOX_TOTAL = { 1: 1284, 2: 642, 3: 318 }
  const boxId = (accountId, role) => accountId * 100 + ROLES.indexOf(role) + 1

  const messages = new Map()
  const summaries = []
  const strip = html => html.replace(/<!--brand-->[\s\S]*?<!--\/brand-->/g, '').replace(/<blockquote[\s\S]*?<\/blockquote>/g, '').replace(/<(br|\/p|\/td|\/tr|\/div)[^>]*>/g, ' ').replace(/<[^>]+>/g, '').replace(/&nbsp;/g, ' ').replace(/\s+/g, ' ').trim()
  const isMine = (address, accountId) => address.email === accounts.find(a => a.id === accountId).email

  for (const t of threads) {
    const inbox = boxId(t.account, 'inbox')
    t.messages.forEach((m, i) => {
      const id = t.id * 10 + i + 1
      const mine = isMine(m.from, t.account)
      const summary = {
        id,
        account_id: t.account,
        mailbox_id: mine ? boxId(t.account, 'sent') : inbox,
        thread_id: t.id,
        subject: (i && !t.subject.startsWith('Invitation') ? 'Re: ' : '') + t.subject,
        from: m.from,
        to: m.to,
        date: m.date,
        preview: strip(m.html).slice(0, 200),
        flags: { seen: m.seen !== false, flagged: !!t.flagged && i === t.messages.length - 1, answered: false, draft: false },
        has_attachments: !!m.attachments?.length
      }
      const known = m.from.name?.includes('Banque') || m.bulk || !mine
      messages.set(id, {
        summary,
        cc: m.cc ?? [],
        reply_to: [],
        message_id: `<${id}@demo.example>`,
        references: [],
        html: m.html,
        is_plain: false,
        text: strip(m.html),
        has_remote_content: !!m.remote,
        attachments: (m.attachments ?? []).map(([filename, mime, size], index) => ({ index, filename, mime, size: Math.round(size) })),
        unsubscribe: m.unsubscribe ? { one_click: 'https://unsubscribe.example/x', web: null, mailto: null } : null,
        bulk: !!m.bulk,
        trust: {
          score: 92,
          level: known ? 'high' : 'medium',
          reasons: [
            { positive: true, text: 'Signature DKIM valide' },
            { positive: true, text: 'Expéditeur déjà connu' }
          ]
        },
        sender_key: m.from.email.split('@')[1],
        images_allowed: !m.remote,
        invite: m.invite ?? null
      })
      summaries.push(summary)
    })
  }

  function threadLine(t) {
    const list = summaries.filter(s => s.thread_id === t.id)
    const latest = list.at(-1)
    const participants = []
    for (const s of list) if (!participants.some(p => p.email === s.from.email)) participants.push(s.from)
    return {
      account_id: t.account,
      mailbox_id: boxId(t.account, 'inbox'),
      thread_id: t.id,
      latest_id: latest.id,
      subject: t.subject,
      from: latest.from,
      to: latest.to,
      participants,
      message_ids: list.map(s => s.id),
      date: latest.date,
      preview: latest.preview,
      count: list.length,
      unread: list.filter(s => !s.flags.seen).length,
      flagged: list.some(s => s.flags.flagged),
      has_attachments: list.some(s => s.has_attachments),
      activity: latest.date,
      snoozed_until: null,
      woken: false,
      category: t.category
    }
  }
  const lines = () => threads.map(threadLine).sort((a, b) => b.activity - a.activity)

  function filtered(list, filter, limit, offset, category) {
    return list
      .filter(t => (filter === 'unread' ? t.unread > 0 : filter === 'flagged' ? t.flagged : true))
      .filter(t => !category || t.category === category)
      .slice(offset, offset + limit)
  }

  function mailboxes(accountId) {
    const mine = lines().filter(t => t.account_id === accountId)
    const waiting = held.filter(h => h.account_id === accountId).reduce((n, h) => n + h.count, 0)
    const count = {
      inbox: [mine.reduce((n, t) => n + t.unread, 0), INBOX_TOTAL[accountId]],
      flagged: [0, mine.filter(t => t.flagged).length + (accountId === demo.pinnedArchive.account ? 1 : 0)],
      drafts: [0, accountId === 2 ? 1 : 0],
      sent: [0, Math.round(INBOX_TOTAL[accountId] * 0.4)],
      archive: [0, INBOX_TOTAL[accountId] * 3],
      junk: [0, 4],
      trash: [0, 12],
      screener: [waiting, waiting],
      refused: [0, 2]
    }
    const special = ROLES.map(role => ({ role, id: boxId(accountId, role), name: NAMES[role] }))
    const custom = FOLDERS[accountId].map((name, i) => ({ role: null, id: accountId * 100 + 50 + i, name }))
    return [...special, ...custom].map(({ role, id, name }) => {
      const [unread, total] = count[role] ?? [0, 20 + ((id * 37) % 140)]
      // Atelier Nova is a Google Workspace account: its folders are labels the user can pick.
      return { id, account_id: accountId, name, display_name: name, delimiter: '/', role, selectable: true, unread, total, server_total: total, user_label: role === null && accountId === 2 }
    })
  }

  // The Maison Brune thread is filed under the Clients label.
  const labelsOf = new Map(summaries.filter(m => m.thread_id === 1).map(m => [m.id, new Set([2 * 100 + 50])]))

  function heldSenders(accountId) {
    return held
      .filter(h => accountId === null || h.account_id === accountId)
      .map((h, i) => ({
        account_id: h.account_id,
        email: h.email,
        name: h.name,
        count: h.count,
        trust: h.trust,
        latest: {
          id: 9000 + i,
          account_id: h.account_id,
          mailbox_id: boxId(h.account_id, 'screener'),
          thread_id: 900 + i,
          subject: h.subject,
          from: { name: h.name, email: h.email },
          to: [{ name: null, email: accounts.find(a => a.id === h.account_id).email }],
          date: h.date,
          preview: h.preview,
          flags: { seen: false, flagged: false, answered: false, draft: false },
          has_attachments: false
        }
      }))
  }

  function accountStats(accountId) {
    const share = accountId === null ? 1 : { 1: 0.5, 2: 0.33, 3: 0.17 }[accountId]
    const scale = n => Math.round(n * share)
    const { months, received, sent } = demo.stats
    const r = received.reduce((a, b) => a + b, 0)
    const s = sent.reduce((a, b) => a + b, 0)
    const p = demo.people
    return {
      total: scale(r) + scale(s),
      received: scale(r),
      sent: scale(s),
      unread: mailboxes(accountId ?? 1)[0].unread,
      flagged: 3,
      with_attachments: scale(812),
      bulk: scale(2304),
      bytes: scale(6.4e8),
      received_30d: scale(498),
      sent_30d: scale(121),
      oldest: Date.UTC(2017, 2, 4) / 1000,
      newest: NOW - 720,
      months: months.map((month, i) => ({ month, received: scale(received[i]), sent: scale(sent[i]) })),
      weekdays: [18, 17, 16, 17, 15, 9, 8].map(v => scale(v * 30)),
      hours: [1, 0, 0, 0, 0, 1, 3, 9, 22, 31, 28, 24, 15, 18, 26, 25, 21, 17, 13, 10, 8, 6, 4, 2].map(v => scale(v * 12)),
      top_senders: [[p.julien, 214], [p.ines, 168], [p.sophie, 121], [p.depliant, 96], [p.helene, 88], [p.marc, 74]].map(([a, n]) => ({ ...a, count: scale(n * 2) })),
      top_recipients: [[p.julien, 132], [p.sophie, 87], [p.ines, 64], [p.lea, 41], [p.marc, 38]].map(([a, n]) => ({ ...a, count: scale(n * 2) })),
      folders: mailboxes(accountId ?? 1).map(m => ({ id: m.id, name: m.display_name, role: m.role, total: m.total, unread: m.unread }))
    }
  }

  // ── Diagnostics (local perf builds; `window.__FACTEUR_PERF__ = true` shows the panel) ──

  function diagnostics() {
    const now = Date.now()
    const conn = (purpose, open, busy, idle) => ({ purpose, open: busy ? null : open, busy, idle_secs: open && !busy ? idle : null })
    const account = (a, i) => ({
      id: a.id,
      email: a.email,
      label: a.label ?? null,
      connections: [conn('sync', i !== 2, i === 0, 4), conn('interactive', true, false, 38), conn('queue', i === 1, false, 95), conn('background', false, false, null)],
      listening_task: true,
      listener: i === 2
        ? { state: 'waiting', since: now - 42_000, notify: null, retry_at: now + 78_000, last_error: 'délai dépassé : connexion au serveur (30 s)', last_error_at: now - 42_000, connections: 4 }
        : { state: 'listening', since: now - (i + 1) * 734_000, notify: i === 0, retry_at: null, last_error: null, last_error_at: null, connections: 1 },
      sync: {
        running_since: i === 0 ? now - 6_400 : null,
        running_scope: i === 0 ? 'inbox' : null,
        running: i === 0 ? 1 : 0,
        last: { started: now - (i + 2) * 61_000, ms: [1840, 612, 31_400][i], scope: 'all', error: i === 2 ? 'délai dépassé : liste des dossiers (60 s)' : null, new_messages: [3, 0, 0][i] },
        count: [41, 37, 12][i],
        failures: [0, 1, 6][i],
        last_success: NOW - (i + 2) * 60
      },
      queue: { pending: [0, 3, 1][i], failed: i === 2 ? 1 : 0, oldest_pending: i ? Math.floor(now / 1000) - [0, 14, 920][i] : null, replaying: i === 1, last_error: i === 2 ? 'NO [OVERQUOTA] Quota exceeded' : null },
      auth_refused: null,
      busy: i === 1 ? ['history'] : [],
      calendar: { state: i === 2 ? 'not_configured' : 'ok', message: null, last_sync: i === 2 ? null : NOW - 240 }
    })
    const busy = [0.62, 0.08, 0.03, 0.41, 0.0, 0.12, 0.05, 0.91].map(b => Math.max(0, Math.min(1, b + (Math.random() - 0.5) * 0.1)))
    return {
      at: now,
      app_version: '0.7.0',
      os: 'windows',
      arch: 'x86_64',
      perf_build: true,
      uptime_secs: 3 * 3600 + 17 * 60 + 9,
      resumed_secs_ago: 5400,
      events_queued: 2,
      event_receivers: 1,
      accounts: accounts.map(account),
      runtime: { workers: 8, alive_tasks: 37, global_queue_depth: 1, worker_busy: busy, busy_span_ms: 2003 },
      store: {
        path: 'C:\\Users\\camille\\AppData\\Roaming\\dev.facteur.mail\\facteur.db',
        file_bytes: 412 * 1048576,
        wal_bytes: 9.4 * 1048576,
        page_size: 4096,
        page_count: 105_472,
        freelist_count: 2_310,
        cache_size: -65536,
        connections: [
          { role: 'writer', busy: true, cache_used_bytes: null, cache_hits: null, cache_misses: null },
          { role: 'reader', busy: false, cache_used_bytes: 6_900_000, cache_hits: 1_840_211, cache_misses: 40_112 },
          { role: 'reader', busy: false, cache_used_bytes: 5_100_000, cache_hits: 1_201_877, cache_misses: 31_009 },
          { role: 'reader', busy: false, cache_used_bytes: 2_300_000, cache_hits: 402_118, cache_misses: 12_440 }
        ]
      },
      memory: { resident_bytes: 186 * 1048576, private_bytes: 142 * 1048576 },
      perf: { window_secs: 300, kinds: [{ kind: 'sql', count: 14, total_ms: 1290, worst: [] }, { kind: 'wait', count: 2, total_ms: 96, worst: [] }, { kind: 'hold', count: 5, total_ms: 610, worst: [] }] }
    }
  }

  const PERF_WHAT = [
    ['sql', 'SELECT t.id, t.subject, t.last_date FROM threads t JOIN messages m ON m.thread_id = t.id WHERE m.mailbox_id = ?1 AND m.deleted = 0 GROUP BY t.id ORDER BY t.last_date DESC LIMIT ?2 OFFSET ?3', 48],
    ['sql', 'UPDATE messages SET flags = ?2 WHERE id = ?1', 34],
    ['sql', 'SELECT rowid FROM search WHERE search MATCH ?1 ORDER BY rank LIMIT 200', 120],
    ['wait', 'writer for crates/core/src/store.rs:2214', 28],
    ['hold', 'writer by crates/core/src/sync.rs:612', 180],
    ['hold', 'reader by crates/core/src/store.rs:1580', 70]
  ]
  let perfSince = 0
  function perfEvents({ since }) {
    const now = Date.now()
    if (!perfSince) {
      perfSince = now
      return Array.from({ length: 60 }, (_, i) => {
        const [kind, what, base] = PERF_WHAT[(i * 7) % PERF_WHAT.length]
        return { at: now - (60 - i) * 4_000, kind, what, ms: base * (0.7 + ((i * 37) % 10) / 6) }
      })
    }
    if (since >= now - 3000 || Math.random() < 0.6) return []
    const [kind, what, base] = PERF_WHAT[Math.floor(Math.random() * PERF_WHAT.length)]
    return [{ at: now, kind, what, ms: base * (0.8 + Math.random()) }]
  }

  const LOG = [
    ['info', 'facteur_app_lib', 'facteur 0.7.0 starting'],
    ['info', 'facteur_core::engine', 'account 1: NOTIFY on, every folder is watched'],
    ['info', 'facteur_core::engine', 'IDLE (account 3) stopped: timed out: connecting to the server (30 s)'],
    ['warn', 'facteur_core::engine', 'sync of account 3: timed out: folder list (60 s)'],
    ['info', 'facteur_core::sync', 'Clients: UIDVALIDITY changed, folder cache reset'],
    ['warn', 'ui', 'perf: unified_threads took 612 ms'],
    ['info', 'facteur_core::engine', 'search of 14 characters (account 2) took 182 ms'],
    ['error', 'facteur_core::ops', 'operation "Move to Archive · Devis cuisine" refused: NO [OVERQUOTA] Quota exceeded (account 3)'],
    ['info', 'facteur_core::engine', 'resumed (wake from sleep or back online): syncing now'],
    ['warn', 'facteur_core::engine', 'replaying operations (account 2): connection closed by the server'],
    ['debug', 'perf', 'hold 180 ms: writer by crates/core/src/sync.rs:612'],
    ['error', 'ui', 'vue (render function): TypeError: Cannot read properties of undefined (reading \'id\') @ http://tauri.localhost/_nuxt/ThreadList.js:1:2045\n    at ThreadList.vue:212\n    at renderList']
  ]
  function readLog({ lines, level, query }) {
    const now = Date.now()
    const levels = level ? level.split(',') : null
    const q = (query || '').toLowerCase()
    const all = Array.from({ length: 400 }, (_, i) => {
      const [lvl, target, message] = LOG[(i * 5 + Math.floor(i / 7)) % LOG.length]
      const m = /(?:compte|account) (\d+)/.exec(message)
      return { ts: now - (400 - i) * 9_000 - (i * 137) % 1000, level: lvl, target, message, account: m ? Number(m[1]) : null }
    })
    return all.filter(l => (!levels || levels.includes(l.level)) && (!q || `${l.target} ${l.message}`.toLowerCase().includes(q))).slice(-lines)
  }

  // ── Commands ──────────────────────────────────────────────────────────────

  const settings = { undo_send: '10', notifications: 'people', saved_searches: JSON.stringify([{ name: 'Projet Maison Brune', query: 'Maison Brune' }, { name: 'Factures', query: 'facture' }]) }
  const handlers = {
    accounts: () => accounts,
    mailboxes: ({ accountId }) => mailboxes(accountId),
    unified_threads: ({ filter, limit, offset, category }) => filtered(lines(), filter, limit, offset, category),
    threads: ({ mailboxId, filter, limit, offset, category }) => {
      const owner = Math.floor(mailboxId / 100)
      return mailboxId === boxId(owner, 'inbox') ? filtered(lines().filter(t => t.account_id === owner), filter, limit, offset, category) : []
    },
    done_threads: () => [],
    category_unread: ({ mailboxId }) => {
      const list = lines().filter(t => mailboxId === null || t.mailbox_id === mailboxId)
      const n = c => list.filter(t => t.category === c && t.unread).length
      return { people: n('people'), notification: n('notification'), newsletter: n('newsletter') }
    },
    thread: ({ threadId }) => summaries.filter(s => s.thread_id === threadId),
    message_labels: ({ ids }) => Object.fromEntries(ids.map(id => [id, [...(labelsOf.get(id) ?? [])]])),
    set_labels: ({ ids, add, remove }) => {
      for (const id of ids) {
        const set = labelsOf.get(id) ?? new Set()
        add.forEach(l => set.add(l))
        remove.forEach(l => set.delete(l))
        labelsOf.set(id, set)
      }
      return 1
    },
    message: ({ id }) => {
      const m = messages.get(id)
      if (!m) throw new Error('message not found')
      return m
    },
    set_flag: ({ ids, flag, value }) => {
      for (const id of ids) {
        const m = messages.get(id)
        if (m) m.summary.flags[flag] = value
      }
    },
    dashboard: () => {
      const flagged = summaries.filter(s => s.flags.flagged)
      const p = demo.pinnedArchive
      flagged.push({ id: 8001, account_id: p.account, mailbox_id: boxId(p.account, 'archive'), thread_id: 801, subject: p.subject, from: p.from, to: [demo.me], date: p.date, preview: '', flags: { seen: true, flagged: true, answered: false, draft: false }, has_attachments: true })
      return { received_today: 14, flagged_count: flagged.length, flagged }
    },
    held_senders: ({ accountId }) => heldSenders(accountId),
    screening: () => [
      { pattern: '@maison-brune.example', decision: 'accept', created_at: NOW - 30 * 86400 },
      { pattern: 'promo@offres-flash.example', decision: 'block', created_at: NOW - 12 * 86400 }
    ],
    follow_ups: () => demo.followUps,
    outbox: () => [],
    ops_status: () => ({ pending: 0, failed: [] }),
    ops_details: () => ({ ops: [], errors: {} }),
    contacts: ({ query }) => {
      const q = (query || '').toLowerCase()
      return demo.contacts.filter(c => !q || `${c.name} ${c.email}`.toLowerCase().includes(q)).slice(0, 8)
    },
    sender_logos: ({ emails }) => Object.fromEntries(emails.map(e => [e, logos[e.split('@')[1]] ?? null])),
    account_stats: ({ accountId }) => accountStats(accountId),
    coverage: () => accounts.map(a => ({ account_id: a.id, local: INBOX_TOTAL[a.id] * 5, server: INBOX_TOTAL[a.id] * 5, bodies: INBOX_TOTAL[a.id] * 5 })),
    backup_status: () => accounts.map(a => ({ account_id: a.id, email: a.email, manifest: { email: a.email, last_backup: NOW - 3 * 3600, messages: INBOX_TOTAL[a.id] * 5, bytes: INBOX_TOTAL[a.id] * 5 * 60000, folders: [] } })),
    get_setting: ({ key }) => settings[key] ?? null,
    set_setting: ({ key, value }) => {
      settings[key] = value
    },
    mail_client_status: () => ({ supported: true, registered: true, default: true }),
    sender_category: () => null,
    save_draft: () => '<draft-1@demo.example>',
    search: ({ accountId, query }) => {
      const q = query.toLowerCase()
      return summaries.filter(s => s.account_id === accountId && `${s.subject} ${s.preview} ${s.from.name}`.toLowerCase().includes(q))
    },
    // Calendars: `window.__FACTEUR_CALENDAR_STATE__` = { <account id>: state } shows the problem states.
    calendars: () => demo.calendar.calendars,
    calendar_status: () => demo.calendar.status.map(st => {
      const state = window.__FACTEUR_CALENDAR_STATE__?.[st.account_id]
      return state ? { ...st, state, message: state === 'error' ? 'Le serveur CalDAV a répondu 401 (identifiants refusés).' : null } : st
    }),
    calendar_events: ({ from, to }) => {
      const on = new Set(demo.calendar.calendars.filter(c => c.enabled).map(c => c.id))
      return demo.calendar.events.filter(e => on.has(e.calendar_id) && e.start < to && (e.end > from || (e.end === e.start && e.start >= from)))
    },
    set_calendar_enabled: ({ id, enabled }) => {
      const c = demo.calendar.calendars.find(x => x.id === id)
      if (c) c.enabled = enabled
    },
    sync_calendars: () => null,
    set_caldav_url: ({ accountId, url }) => {
      const st = demo.calendar.status.find(x => x.account_id === accountId)
      if (st) st.caldav_url = url
    },
    // Local AI: turned on, the balanced model and dictation ready.
    ai_status: () => ({
      available: true, model: 'qwen3.5-4b', file: null, ready: true, download: null, error: null, memory: 16e9, fitting: 'qwen3.5-4b',
      models: [
        { id: 'qwen3.5-2b', name: 'Qwen 3.5 2B', tier: 'light', file: 'Qwen3.5-2B-Q4_K_M.gguf', bytes: 1280835840, memory: 4 },
        { id: 'qwen3.5-4b', name: 'Qwen 3.5 4B', tier: 'balanced', file: 'Qwen3.5-4B-Q4_K_M.gguf', bytes: 2740937888, memory: 8 },
        { id: 'qwen3.5-9b', name: 'Qwen 3.5 9B', tier: 'precise', file: 'Qwen3.5-9B-Q4_K_M.gguf', bytes: 5680522464, memory: 16 }
      ],
      dictation: true, dictation_ready: true,
      dictation_model: { id: 'whisper-turbo', name: 'Whisper large-v3 turbo', tier: 'dictation', file: 'ggml-large-v3-turbo-q5_0.bin', bytes: 574041195, memory: 4 }
    }),
    ai_summarize: () => [
      '- Sophie (Maison Brune) valide les maquettes v2 et demande deux changements : un bouton « Ajouter au panier » plus visible sur mobile et un encart « Atelier ouvert le samedi » sur l\'accueil.',
      '- Julien a intégré les deux dans la v3 (bouton du panier fixé en bas d\'écran, encart sous le bandeau avec horaires et plan).',
      '- Il attend ton accord pour lancer l\'intégration jeudi.',
      '- Mise en ligne visée le 2 novembre.'
    ].join('\n'),
    ai_find_event: () => null,
    event_repeat: () => ({ repeat: null, custom: false }),
    connection_advice: () => [],
    perf_enabled: () => !!window.__FACTEUR_PERF__,
    perf_events: perfEvents,
    diagnostics,
    read_log: readLog,
    open_logs: () => null,
    'plugin:app|version': () => '0.7.0',
    'plugin:app|name': () => 'facteur',
    'plugin:path|resolve_directory': () => 'C:\\Users\\camille\\Documents',
    'plugin:path|join': ({ paths }) => paths.join('\\'),
    'plugin:dialog|confirm': () => true,
    'plugin:dialog|ask': () => true,
    'plugin:dialog|message': () => null
  }

  // ── Tauri internals ───────────────────────────────────────────────────────

  const callbacks = new Map()
  let nextId = 1
  const unknown = new Set()

  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main', windowLabel: 'main' } },
    plugins: {},
    transformCallback(callback, once = false) {
      const id = nextId++
      callbacks.set(id, { callback, once })
      return id
    },
    unregisterCallback(id) {
      callbacks.delete(id)
    },
    runCallback(id, data) {
      const entry = callbacks.get(id)
      if (!entry) return
      if (entry.once) callbacks.delete(id)
      entry.callback(data)
    },
    callbacks,
    convertFileSrc: (path, protocol = 'asset') => `${protocol}://localhost/${encodeURIComponent(path)}`,
    async invoke(cmd, args = {}) {
      if (cmd === 'plugin:event|listen') return nextId++
      if (cmd.startsWith('plugin:event|')) return null
      if (cmd.startsWith('plugin:window|') || cmd.startsWith('plugin:webview|')) return null
      const handler = handlers[cmd]
      if (handler) return structuredClone(handler(args ?? {}))
      if (!unknown.has(cmd)) {
        unknown.add(cmd)
        console.debug('[mock] unhandled command', cmd)
      }
      // Lists for list-looking commands, nothing for the rest.
      return /s$|_busy$|^(rules|blocked_senders|trusted_images|list_backups)$/.test(cmd) ? [] : null
    }
  }
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  window.__FACTEUR_MOCK_UNKNOWN__ = unknown
})()
