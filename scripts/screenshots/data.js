// Demo data for the website screenshots (scripts/screenshots/capture.mjs).
// Everything here is invented: people, companies, domains (the reserved `.example` TLD and
// example.org / .com / .net) and logos. Loaded as a browser init script before mock.js, it
// defines `window.__FACTEUR_DEMO__`, which the mock turns into engine answers.
;(() => {
  // The clock the screenshots are taken at: Tuesday 13 October 2026, 10:24 in Paris.
  // capture.mjs starts the page's clock at the same instant.
  const NOW = Date.UTC(2026, 9, 13, 8, 24) / 1000
  const MIN = 60
  const HOUR = 3600
  const DAY = 86400
  /** A moment `days` before today, at `hh:mm` (Paris time, summer offset). */
  const at = (days, hh, mm = 0) => Date.UTC(2026, 9, 13 - days, hh - 2, mm) / 1000

  // ── Accounts ──────────────────────────────────────────────────────────────

  const me = { name: 'Camille Laurent', email: 'camille.laurent@example.org' }
  const meWork = { name: 'Camille Laurent', email: 'camille@atelier-nova.example' }
  const meClub = { name: 'Camille Laurent', email: 'tresorerie@roues-libres.example' }

  const server = { host: 'imap.example.org', port: 993, security: 'tls' }
  const smtp = { host: 'smtp.example.org', port: 465, security: 'tls' }
  const account = (id, label, who, color, signature) => ({
    id,
    name: who.name,
    email: who.email,
    username: who.email,
    imap: server,
    smtp,
    auth: { kind: 'password' },
    last_sync: NOW - 2 * MIN,
    signature,
    avatar: null,
    label,
    identities: [],
    color,
    avatar_custom: false,
    screener_since: at(40, 9)
  })
  const accounts = [
    account(1, 'Personnel', me, 'violet', '<p>Camille</p>'),
    account(2, 'Atelier Nova', meWork, 'blue', '<p>Camille Laurent<br>Directrice artistique · Atelier Nova</p>'),
    account(3, 'Les Roues Libres', meClub, 'emerald', '<p>Camille Laurent<br>Trésorière · Les Roues Libres</p>')
  ]

  // ── People and brands ─────────────────────────────────────────────────────

  const p = {
    julien: { name: 'Julien Mercier', email: 'julien.mercier@atelier-nova.example' },
    nora: { name: 'Nora Benali', email: 'nora.benali@atelier-nova.example' },
    thomas: { name: 'Thomas Roy', email: 'thomas.roy@atelier-nova.example' },
    sophie: { name: 'Sophie Andrieu', email: 'sophie.andrieu@maison-brune.example' },
    ines: { name: 'Inès Garnier', email: 'ines.garnier@example.net' },
    marc: { name: 'Marc Delorme', email: 'marc.delorme@example.com' },
    helene: { name: 'Hélène Laurent', email: 'helene.laurent@example.org' },
    lea: { name: 'Léa Bonnet', email: 'lea.bonnet@example.net' },
    antoine: { name: 'Antoine Moreau', email: 'antoine.moreau@example.com' },
    horizon: { name: 'Banque Horizon', email: 'releves@banque-horizon.example' },
    ferrovia: { name: 'Ferrovia', email: 'billets@ferrovia.example' },
    depliant: { name: 'Le Petit Dépliant', email: 'bonjour@lepetitdepliant.example' },
    ondine: { name: 'Boutique Ondine', email: 'commandes@ondine.example' },
    nimbus: { name: 'Nimbus Docs', email: 'notifications@nimbus.example' },
    pedale: { name: 'Pédale Douce', email: 'lettre@pedaledouce.example' },
    frequence: { name: 'Mutuelle Fréquence', email: 'service@frequence-mutuelle.example' },
    valfleury: { name: 'Mairie de Valfleury', email: 'infos@valfleury.example' },
    typo: { name: 'Typographie Hebdo', email: 'hebdo@typo-hebdo.example' }
  }

  const logo = (bg, glyph) => 'data:image/svg+xml,' + encodeURIComponent(
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="14" fill="${bg}"/>${glyph}</svg>`
  )
  const letter = (c, color = '#fff') =>
    `<text x="32" y="43" text-anchor="middle" font-family="Georgia, serif" font-size="30" font-weight="700" fill="${color}">${c}</text>`
  const logos = {
    'banque-horizon.example': logo('#1d4ed8', '<circle cx="32" cy="38" r="12" fill="#fbbf24"/><rect x="12" y="38" width="40" height="4" rx="2" fill="#fff"/>'),
    'ferrovia.example': logo('#be123c', '<path d="M14 40h36M18 46h28" stroke="#fff" stroke-width="4" stroke-linecap="round"/><rect x="20" y="16" width="24" height="20" rx="6" fill="#fff"/>'),
    'lepetitdepliant.example': logo('#f59e0b', '<path d="M16 20l10 4 12-4 10 4v20l-10-4-12 4-10-4z" fill="#fff"/>'),
    'ondine.example': logo('#0f766e', '<path d="M12 36c6-6 10-6 14 0s8 6 14 0 8-6 12 0" stroke="#fff" stroke-width="4" fill="none" stroke-linecap="round"/>'),
    'nimbus.example': logo('#6366f1', '<path d="M22 42a8 8 0 0 1 0-16 11 11 0 0 1 21-2 8 8 0 0 1 1 18z" fill="#fff"/>'),
    'pedaledouce.example': logo('#16a34a', '<circle cx="22" cy="38" r="8" stroke="#fff" stroke-width="3.5" fill="none"/><circle cx="44" cy="38" r="8" stroke="#fff" stroke-width="3.5" fill="none"/><path d="M22 38l8-14h10l4 14" stroke="#fff" stroke-width="3.5" fill="none" stroke-linejoin="round"/>'),
    'frequence-mutuelle.example': logo('#db2777', '<path d="M12 34h10l4-10 6 20 5-14 3 4h12" stroke="#fff" stroke-width="4" fill="none" stroke-linecap="round" stroke-linejoin="round"/>'),
    'valfleury.example': logo('#0369a1', '<path d="M18 46V28l14-10 14 10v18z" fill="#fff"/><rect x="28" y="34" width="8" height="12" fill="#0369a1"/>'),
    'typo-hebdo.example': logo('#18181b', letter('Aa')),
    'studio-pixelle.example': logo('#ea580c', '<rect x="18" y="18" width="12" height="12" fill="#fff"/><rect x="34" y="18" width="12" height="12" fill="#fff" opacity=".7"/><rect x="18" y="34" width="12" height="12" fill="#fff" opacity=".7"/><rect x="34" y="34" width="12" height="12" fill="#fff"/>'),
    'agence-ruche.example': logo('#ca8a04', '<path d="M32 16l12 7v14l-12 7-12-7V23z" fill="#fff"/>'),
    'maison-brune.example': logo('#78350f', letter('B', '#fde68a'))
  }

  // ── Message bodies ────────────────────────────────────────────────────────

  const para = (...lines) => lines.map(l => `<p>${l}</p>`).join('')
  const quote = html => `<blockquote class="facteur-quote">${html}</blockquote>`
  const sig = (name, role) => `<p style="color:#71717a;font-size:13px;margin-top:18px">${name}${role ? `<br>${role}` : ''}</p>`

  /** A newsletter laid out like the real thing: header band, title, a few cards, footer. */
  function newsletter({ brand, color, title, intro, items, footer }) {
    const cards = items.map(([h, t, tag]) => `
      <tr><td style="padding:0 28px 18px">
        <table width="100%" cellpadding="0" cellspacing="0" style="border:1px solid #e7e5e4;border-radius:12px">
          <tr><td style="padding:16px 18px">
            <div style="font:600 11px/1 system-ui,sans-serif;letter-spacing:.08em;text-transform:uppercase;color:${color}">${tag}</div>
            <div style="font:700 17px/1.35 Georgia,serif;color:#1c1917;margin:8px 0 6px">${h}</div>
            <div style="font:14px/1.55 system-ui,sans-serif;color:#57534e">${t}</div>
          </td></tr>
        </table>
      </td></tr>`).join('')
    return `
      <table width="100%" cellpadding="0" cellspacing="0" style="background:#f5f5f4;padding:24px 0">
        <tr><td align="center">
          <table width="600" cellpadding="0" cellspacing="0" style="max-width:600px;width:100%;background:#fff;border-radius:16px;overflow:hidden">
            <!--brand--><tr><td style="background:${color};padding:22px 28px;font:700 20px/1 Georgia,serif;color:#fff">${brand}</td></tr><!--/brand-->
            <tr><td style="padding:28px 28px 8px;font:700 26px/1.25 Georgia,serif;color:#1c1917">${title}</td></tr>
            <tr><td style="padding:0 28px 22px;font:15px/1.6 system-ui,sans-serif;color:#44403c">${intro}</td></tr>
            ${cards}
            <tr><td style="padding:8px 28px 26px;font:12px/1.5 system-ui,sans-serif;color:#a8a29e">${footer}</td></tr>
          </table>
        </td></tr>
      </table>`
  }

  /** A transactional notification: logo band, one block of facts, one button. */
  function notice({ brand, color, title, text, rows, button }) {
    const facts = (rows || []).map(([k, v]) => `
      <tr><td style="padding:7px 0;color:#71717a">${k}</td><td style="padding:7px 0;text-align:right;color:#18181b;font-weight:600">${v}</td></tr>`).join('')
    return `
      <table width="100%" cellpadding="0" cellspacing="0" style="background:#f4f4f5;padding:24px 0">
        <tr><td align="center">
          <table width="560" cellpadding="0" cellspacing="0" style="max-width:560px;width:100%;background:#fff;border-radius:14px;font:14.5px/1.6 system-ui,sans-serif;color:#3f3f46">
            <!--brand--><tr><td style="padding:22px 28px;border-bottom:3px solid ${color};font:700 18px/1 system-ui,sans-serif;color:${color}">${brand}</td></tr><!--/brand-->
            <tr><td style="padding:24px 28px 4px;font:700 20px/1.3 system-ui,sans-serif;color:#18181b">${title}</td></tr>
            <tr><td style="padding:6px 28px 10px">${text}</td></tr>
            ${facts ? `<tr><td style="padding:4px 28px 12px"><table width="100%" style="border-top:1px solid #e4e4e7;font-size:14px">${facts}</table></td></tr>` : ''}
            ${button ? `<tr><td style="padding:10px 28px 28px"><a href="https://${button[1]}" style="display:inline-block;background:${color};color:#fff;text-decoration:none;font-weight:600;padding:11px 20px;border-radius:10px">${button[0]}</a></td></tr>` : ''}
          </table>
        </td></tr>
      </table>`
  }

  // ── Threads ───────────────────────────────────────────────────────────────
  // `account`: 1 personal, 2 work, 3 club. `category`: smart inbox tab. Messages oldest first;
  // `seen: false` marks an unread one. Attachment sizes are in bytes.

  const KB = 1024
  const MB = 1024 * 1024
  const threads = [
    {
      id: 1, account: 2, category: 'people', subject: 'Maison Brune — maquettes v3',
      messages: [
        {
          from: p.julien, to: [meWork], cc: [p.nora], date: at(1, 16, 5),
          html: para('Salut Camille,', 'Voici la v2 des maquettes pour Maison Brune : la page d\'accueil, la fiche produit et le panier. J\'ai repris la typo des titres comme tu le suggérais.', 'Dis-moi si on peut l\'envoyer à Sophie ce soir.') + sig('Julien'),
          attachments: [['maison-brune-v2.pdf', 'application/pdf', 2.1 * MB]]
        },
        {
          from: meWork, to: [p.sophie], cc: [p.julien], date: at(1, 18, 32),
          html: para('Bonjour Sophie,', 'Vous trouverez ci-joint la deuxième version des maquettes. Nous avons simplifié la navigation et mis vos photos d\'atelier en avant dès l\'accueil.', 'Bonne soirée,') + sig('Camille Laurent', 'Directrice artistique · Atelier Nova')
        },
        {
          from: p.sophie, to: [meWork], cc: [p.julien], date: at(0, 8, 41),
          html: para('Bonjour Camille, bonjour Julien,', 'Merci, c\'est vraiment très réussi ! Deux petites choses :', '1. Le bouton « Ajouter au panier » pourrait-il être plus visible sur mobile ?<br>2. Nous aimerions ajouter un encart « Atelier ouvert le samedi » sur l\'accueil.', 'Pour le reste, c\'est parfait pour nous. Belle journée,') + sig('Sophie Andrieu', 'Maison Brune — Céramiques')
        },
        {
          from: p.julien, to: [meWork, p.sophie], cc: [p.nora], date: at(0, 10, 12),
          html: para('Bonjour Sophie,', 'C\'est intégré dans la v3 ci-jointe :', '• le bouton du panier reste visible en bas de l\'écran sur mobile, en terracotta ;<br>• l\'encart « Atelier ouvert le samedi » apparaît sous le bandeau, avec les horaires et le plan d\'accès.', 'Camille, si c\'est bon pour toi, on peut lancer l\'intégration jeudi et viser une mise en ligne le 2 novembre.', 'Bonne journée à toutes les deux,') + sig('Julien Mercier', 'Designer · Atelier Nova') +
            quote(para('Le 13 oct. à 08:41, Sophie Andrieu a écrit :', 'Merci, c\'est vraiment très réussi ! Deux petites choses…')),
          attachments: [['maison-brune-v3.pdf', 'application/pdf', 2.4 * MB], ['accueil-mobile.png', 'image/png', 612 * KB], ['fiche-produit.png', 'image/png', 548 * KB]]
        }
      ]
    },
    {
      id: 2, account: 3, category: 'people', subject: 'Sortie du dimanche 18 : parcours et covoiturage',
      messages: [
        { from: p.marc, to: [meClub, p.ines], date: at(1, 20, 14), html: para('Salut à toutes les deux,', 'Pour dimanche, je propose la boucle des étangs (62 km, 480 m de dénivelé). Départ 8 h 30 devant la salle des fêtes ?') + sig('Marc') },
        { from: meClub, to: [p.marc, p.ines], date: at(1, 21, 2), html: para('Parfait pour moi. Je peux prendre deux vélos sur le porte-vélo.') + sig('Camille') },
        {
          from: p.ines, to: [meClub, p.marc], date: at(0, 9, 47), seen: false,
          html: para('Super ! J\'ai mis le parcours en pièce jointe. Pause café prévue à Saint-Aubin, au km 34.', 'Pour le covoiturage : Camille prend Léo et Hugo, je prends les deux nouveaux. Pensez aux chambres à air de rechange !') + sig('Inès'),
          attachments: [['boucle-des-etangs.gpx', 'application/gpx+xml', 184 * KB]]
        }
      ]
    },
    {
      id: 3, account: 1, category: 'notification', subject: 'Votre relevé d\'octobre est disponible',
      messages: [{
        from: p.horizon, to: [me], date: at(0, 9, 31), seen: false, bulk: true,
        html: notice({ brand: 'Banque Horizon', color: '#1d4ed8', title: 'Votre relevé est disponible', text: 'Bonjour Camille Laurent, le relevé de votre compte courant pour la période du 1<sup>er</sup> au 30 septembre 2026 est disponible dans votre espace client.', rows: [['Compte', 'Compte courant ••• 4821'], ['Période', 'Septembre 2026']], button: ['Consulter mon relevé', 'banque-horizon.example/releves'] })
      }]
    },
    {
      id: 4, account: 2, category: 'people', subject: 'Invitation : Point hebdo produit — mer. 14 oct. 14:00',
      messages: [{
        from: p.thomas, to: [meWork, p.julien, p.nora], date: at(0, 9, 5), seen: false,
        html: para('Point hebdomadaire : avancement Maison Brune, planning de novembre, questions diverses.'),
        invite: {
          method: 'REQUEST', uid: 'hebdo-2026-10-14@atelier-nova.example', summary: 'Point hebdo produit',
          location: 'Salle Lumière, 2e étage', description: 'Avancement Maison Brune, planning de novembre.',
          start: at(-1, 14), end: at(-1, 15), all_day: false, tzid: 'Europe/Paris', organizer: p.thomas,
          attendees: [{ address: meWork, partstat: 'NEEDS-ACTION' }, { address: p.julien, partstat: 'ACCEPTED' }, { address: p.nora, partstat: 'TENTATIVE' }],
          recurring: true, cancelled: false, sequence: 0, my_response: null
        }
      }]
    },
    {
      id: 5, account: 1, category: 'people', subject: 'Repas de dimanche',
      messages: [{
        from: p.helene, to: [me], date: at(0, 8, 52), seen: false,
        html: para('Ma chérie,', 'Ton frère vient finalement dimanche avec les enfants. Je fais le gratin de courge de mamie, tu peux apporter le dessert ?', 'On se dit midi et demi. Je t\'embrasse,') + sig('Maman')
      }]
    },
    {
      id: 6, account: 1, category: 'newsletter', subject: 'Cinq balades d\'automne à moins d\'une heure de chez vous',
      messages: [{
        from: p.depliant, to: [me], date: at(0, 7, 30), seen: false, bulk: true, remote: true,
        html: newsletter({
          brand: 'Le Petit Dépliant', color: '#d97706', title: 'Cinq balades d\'automne à moins d\'une heure de chez vous',
          intro: 'Les forêts rougissent, les chemins sentent le champignon : c\'est le moment de sortir. Notre sélection de la semaine, testée à pied par la rédaction.',
          items: [
            ['La forêt des Trois Pignons, entre sable et rochers', 'Une boucle de 9 km, accessible en train, avec un pique-nique au sommet du Rocher Canon.', 'Randonnée'],
            ['Le long du canal, de péniche en guinguette', 'Un parcours plat de 12 km, parfait à vélo, ponctué de deux écluses et d\'un marché bio.', 'À vélo'],
            ['Les vergers de Valfleury', 'Cueillette de pommes jusqu\'au 31 octobre, jus pressé sur place.', 'En famille']
          ],
          footer: 'Vous recevez cette lettre car vous êtes abonnée au Petit Dépliant. Se désabonner · Gérer mes préférences'
        }),
        unsubscribe: true
      }]
    },
    {
      id: 7, account: 1, category: 'notification', subject: 'Votre billet : Paris → Annecy, samedi 24 octobre', flagged: true,
      messages: [{
        from: p.ferrovia, to: [me], date: at(1, 19, 40), bulk: true,
        html: notice({ brand: 'Ferrovia', color: '#be123c', title: 'Bon voyage, Camille !', text: 'Votre réservation est confirmée. Votre billet est joint à ce message et disponible dans l\'application.', rows: [['Aller', 'Sam. 24 oct. · 08:12 → 11:58'], ['Voiture 14, place 82', 'Côté fenêtre'], ['Référence', 'QZ7-K4M']], button: ['Gérer ma réservation', 'ferrovia.example/reservations'] }),
        attachments: [['billet-QZ7K4M.pdf', 'application/pdf', 186 * KB]]
      }]
    },
    {
      id: 8, account: 1, category: 'people', subject: 'Photos du week-end à Étretat',
      messages: [
        { from: me, to: [p.lea], date: at(1, 20, 3), html: para('Tu peux m\'envoyer les photos de la falaise ? Celle avec le coucher de soleil surtout !') + sig('Camille') },
        {
          from: p.lea, to: [me], date: at(1, 21, 14), seen: false,
          html: para('Les voilà ! Je t\'ai mis les trois meilleures, les autres sont dans l\'album partagé.', 'On remet ça au printemps ?') + sig('Léa'),
          attachments: [['etretat-falaise.jpg', 'image/jpeg', 3.2 * MB], ['coucher-de-soleil.jpg', 'image/jpeg', 2.8 * MB], ['port.jpg', 'image/jpeg', 2.5 * MB]]
        }
      ]
    },
    {
      id: 9, account: 3, category: 'notification', subject: 'Marc a partagé « Budget 2027 » avec vous',
      messages: [{
        from: p.nimbus, to: [meClub], date: at(1, 17, 2), bulk: true,
        html: notice({ brand: 'Nimbus Docs', color: '#6366f1', title: 'Marc Delorme a partagé un tableur avec vous', text: '« Budget 2027 — Les Roues Libres » : vous pouvez maintenant le consulter et le modifier.', button: ['Ouvrir le document', 'nimbus.example/d/budget-2027'] })
      }]
    },
    {
      id: 10, account: 2, category: 'people', subject: 'Devis signé — site Maison Brune', flagged: true,
      messages: [{
        from: p.sophie, to: [meWork], date: at(1, 15, 20),
        html: para('Bonjour Camille,', 'Voici le devis signé. Nous avons hâte de voir le site en ligne !', 'Bien cordialement,') + sig('Sophie Andrieu', 'Maison Brune — Céramiques'),
        attachments: [['devis-2026-118-signe.pdf', 'application/pdf', 312 * KB]]
      }]
    },
    {
      id: 11, account: 1, category: 'notification', subject: 'Votre commande a été expédiée',
      messages: [{
        from: p.ondine, to: [me], date: at(1, 11, 8), bulk: true,
        html: notice({ brand: 'Boutique Ondine', color: '#0f766e', title: 'Votre colis est en route', text: 'Bonne nouvelle : votre commande n° 58213 a quitté notre atelier. Livraison prévue jeudi 15 octobre.', rows: [['Plaid en laine, vert sauge', '1 × 69,00 €'], ['Livraison', 'Point relais']], button: ['Suivre mon colis', 'ondine.example/suivi'] })
      }]
    },
    {
      id: 12, account: 3, category: 'people', subject: 'Cotisations 2026-2027 : point au 11 octobre',
      messages: [
        { from: p.marc, to: [meClub], date: at(2, 18, 30), html: para('Camille,', 'Voici le tableau des cotisations à jour : 47 adhérents ont réglé sur 58. Je relance les autres cette semaine ?') + sig('Marc'), attachments: [['cotisations-2026-2027.xlsx', 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet', 46 * KB]] },
        { from: meClub, to: [p.marc], date: at(2, 19, 5), html: para('Merci Marc ! Oui, relance-les, je ferai le point en réunion de bureau.') + sig('Camille') }
      ]
    },
    {
      id: 13, account: 3, category: 'newsletter', subject: 'Entretenir sa chaîne en cinq minutes',
      messages: [{
        from: p.pedale, to: [meClub], date: at(2, 8, 0), bulk: true, unsubscribe: true,
        html: newsletter({ brand: 'Pédale Douce', color: '#16a34a', title: 'Entretenir sa chaîne en cinq minutes', intro: 'Une chaîne propre, c\'est un vélo plus silencieux et des pignons qui durent deux fois plus longtemps.', items: [['Le bon geste : dégraisser, sécher, huiler', 'Trois étapes et un chiffon suffisent, à faire toutes les quatre sorties.', 'Atelier'], ['Pneus d\'hiver : faut-il passer au 32 mm ?', 'Notre test sur route mouillée.', 'Matériel']], footer: 'Pédale Douce, le magazine du vélo au quotidien. Se désabonner' })
      }]
    },
    {
      id: 14, account: 1, category: 'people', subject: 'Concert jeudi ?',
      messages: [
        { from: p.antoine, to: [me], date: at(3, 22, 41), html: para('Il me reste une place pour Les Lucioles jeudi au Trianon, ça te dit ?') + sig('Antoine') },
        { from: me, to: [p.antoine], date: at(3, 23, 2), html: para('Carrément ! On dîne avant ?') + sig('Camille') },
        { from: p.antoine, to: [me], date: at(2, 10, 15), html: para('Top, je réserve chez Mona pour 19 h.') + sig('Antoine') }
      ]
    },
    {
      id: 15, account: 1, category: 'notification', subject: 'Remboursement effectué',
      messages: [{ from: p.frequence, to: [me], date: at(4, 14, 22), bulk: true, html: notice({ brand: 'Mutuelle Fréquence', color: '#db2777', title: 'Un remboursement vient d\'être versé', text: 'Le remboursement de votre consultation du 2 octobre a été versé sur votre compte.', rows: [['Montant', '18,40 €']] }) }]
    },
    {
      id: 16, account: 2, category: 'people', subject: 'Planning des congés de fin d\'année',
      messages: [{ from: p.nora, to: [meWork, p.julien, p.thomas], date: at(4, 11, 47), html: para('Bonjour à tous,', 'Pouvez-vous indiquer vos dates de congés de décembre dans le tableau partagé d\'ici vendredi prochain ? Merci !') + sig('Nora') }]
    },
    {
      id: 17, account: 1, category: 'newsletter', subject: 'Marché de Noël, travaux rue des Lilas et nouveaux horaires',
      messages: [{ from: p.valfleury, to: [me], date: at(5, 9, 0), bulk: true, unsubscribe: true, html: newsletter({ brand: 'Valfleury', color: '#0369a1', title: 'Les nouvelles de Valfleury', intro: 'Ce mois-ci : les inscriptions au marché de Noël, les travaux de la rue des Lilas et les nouveaux horaires de la médiathèque.', items: [['Marché de Noël : les inscriptions sont ouvertes', 'Exposants, inscrivez-vous avant le 15 novembre.', 'Vie locale'], ['Rue des Lilas : travaux du 19 au 30 octobre', 'La circulation sera alternée.', 'Travaux']], footer: 'Mairie de Valfleury · Se désabonner' }) }]
    },
    {
      id: 18, account: 2, category: 'notification', subject: 'Julien a commenté « Charte graphique v2 »',
      messages: [{ from: p.nimbus, to: [meWork], date: at(5, 16, 30), bulk: true, html: notice({ brand: 'Nimbus Docs', color: '#6366f1', title: 'Nouveau commentaire', text: '« On garde le terracotta pour les boutons ? » — Julien Mercier, sur Charte graphique v2.', button: ['Répondre', 'nimbus.example/d/charte-v2'] }) }]
    },
    {
      id: 19, account: 2, category: 'newsletter', subject: 'Les fontes variables, deux ans après',
      messages: [{ from: p.typo, to: [meWork], date: at(6, 7, 45), bulk: true, unsubscribe: true, html: newsletter({ brand: 'Typographie Hebdo', color: '#18181b', title: 'Les fontes variables, deux ans après', intro: 'Poids, chasse, contraste optique : ce que les fontes variables ont vraiment changé sur le web.', items: [['Cinq familles libres à essayer', 'Notre sélection pour les interfaces et l\'éditorial.', 'Sélection']], footer: 'Typographie Hebdo · Se désabonner' }) }]
    }
  ]

  // ── New senders waiting in the screener ───────────────────────────────────

  const held = [
    {
      account_id: 3, name: 'Paul Verdier', email: 'paul.verdier@example.net', count: 1, trust: 'high',
      subject: 'Inscription au club pour la saison', date: at(0, 8, 12),
      preview: 'Bonjour, je viens d\'arriver à Valfleury et j\'aimerais rejoindre le club. Les sorties du dimanche sont-elles ouvertes aux débutants ?'
    },
    {
      account_id: 1, name: 'Studio Pixelle', email: 'hello@studio-pixelle.example', count: 3, trust: 'medium',
      subject: '-30 % sur vos tirages photo jusqu\'à dimanche', date: at(1, 12, 0),
      preview: 'Vos plus beaux souvenirs méritent mieux qu\'un écran : tirages, livres photo et cadres à prix doux cette semaine.'
    },
    {
      account_id: 1, name: 'Banque Horizon', email: 'securite@banque-horlzon.example', count: 1, trust: 'low',
      subject: 'Action requise : votre compte sera suspendu', date: at(1, 6, 48),
      preview: 'Nous avons détecté une activité inhabituelle. Confirmez vos informations sous 24 heures pour éviter la suspension.'
    },
    {
      account_id: 2, name: 'Agence Ruche', email: 'contact@agence-ruche.example', count: 2, trust: 'medium',
      subject: 'Proposition de collaboration', date: at(2, 15, 30),
      preview: 'Bonjour, nous suivons le travail d\'Atelier Nova depuis un moment et aimerions vous proposer un projet commun.'
    }
  ]

  // ── Calendars ─────────────────────────────────────────────────────────────

  const calendars = [
    { id: 1, account_id: 1, name: 'Personnel', color: null, source: 'caldav', enabled: true, read_only: false, error: null },
    { id: 2, account_id: 1, name: 'Anniversaires', color: '#ec4899', source: 'caldav', enabled: true, read_only: true, error: null },
    { id: 3, account_id: 1, name: 'Jours fériés', color: '#64748b', source: 'caldav', enabled: true, read_only: true, error: null },
    { id: 4, account_id: 2, name: 'Atelier Nova', color: null, source: 'google', enabled: true, read_only: false, error: null },
    { id: 5, account_id: 2, name: 'Projets clients', color: '#0d9488', source: 'google', enabled: true, read_only: false, error: null },
    { id: 6, account_id: 2, name: 'Congés équipe', color: '#f59e0b', source: 'google', enabled: false, read_only: true, error: null },
    { id: 7, account_id: 3, name: 'Sorties club', color: null, source: 'caldav', enabled: true, read_only: false, error: null }
  ]
  const calendarStatus = [
    { account_id: 1, state: 'ok', message: null, caldav_url: 'https://dav.example.org/calendars/camille/', last_sync: NOW - 4 * MIN, synced_from: NOW - 60 * DAY, synced_to: NOW + 365 * DAY },
    { account_id: 2, state: 'ok', message: null, caldav_url: null, last_sync: NOW - 4 * MIN, synced_from: NOW - 60 * DAY, synced_to: NOW + 365 * DAY },
    { account_id: 3, state: 'ok', message: null, caldav_url: 'https://cloud.roues-libres.example/remote.php/dav/', last_sync: NOW - 4 * MIN, synced_from: NOW - 60 * DAY, synced_to: NOW + 365 * DAY }
  ]

  /** Events, `day` days after today (negative: before), at `hh:mm` for `minutes`. */
  let eventId = 0
  const gcal = 'https://calendar.google.com/calendar/event?eid=ZGVtbw'
  // In the page's local time (capture.mjs runs it in Europe/Paris): the clocks change on 25 October.
  const local = (day, hh = 0, mm = 0) => new Date(2026, 9, 13 + day, hh, mm).getTime() / 1000
  const event = (calendar, summary, day, hh, mm, minutes, extra = {}) => {
    const cal = calendars.find(c => c.id === calendar)
    const start = local(day, hh, mm)
    return {
      id: ++eventId, calendar_id: calendar, account_id: cal.account_id, uid: `demo-${eventId}@example.org`, instance: '', busy: true,
      summary, location: null, description: null, start, end: start + minutes * MIN, all_day: false,
      status: 'confirmed', organizer: null, url: cal.source === 'google' ? gcal : null,
      recurring: false, editable: true, response: null, respondable: false, attendees: [], meeting: null, reminders: [], ...extra
    }
  }
  /** All-day events: local midnights, the end exclusive. */
  const allDay = (calendar, summary, day, days = 1, extra = {}) => event(calendar, summary, day, 0, 0, 0, { all_day: true, end: local(day + days), ...extra })
  // Today is Tuesday: Monday is day -1.
  const weekly = (calendar, summary, weekday, hh, mm, minutes, extra) =>
    [-14, -7, 0, 7, 14, 21].map(w => event(calendar, summary, w + weekday, hh, mm, minutes, extra))

  const events = [
    // Personnel
    ...weekly(1, 'Yoga', -1, 19, 0, 75, { location: 'Studio Prana, 8 rue Sainte-Catherine' }),
    event(1, 'Déjeuner avec Inès', 0, 12, 30, 75, { location: 'Café des Arts, place des Terreaux', organizer: 'Inès Garnier <ines.garnier@example.net>' }),
    event(1, 'Dentiste — Dr Marchand', 1, 17, 30, 45, { location: '14 avenue des Frères Lumière', description: 'Détartrage annuel. Apporter la carte de mutuelle.' }),
    event(1, 'Dîner chez Hélène et Marc', 3, 20, 0, 180, { location: 'Chez Hélène' }),
    event(1, 'Marché et courses', 4, 10, 0, 90),
    event(1, 'Repas chez maman', 5, 12, 30, 180, { description: 'Apporter le dessert (tarte aux pommes ?).' }),
    event(1, 'Train Paris → Annecy', 11, 8, 12, 226, { location: 'Gare de Lyon', description: 'Voiture 14, place 82. Référence QZ7-K4M.' }),
    allDay(1, 'Week-end à Annecy', 11, 2),
    allDay(2, 'Anniversaire de Léa', 4),
    allDay(2, 'Anniversaire de papa', 16),
    allDay(3, 'Toussaint', 19),
    allDay(3, 'Armistice', 29),
    // Atelier Nova
    ...weekly(4, 'Point hebdo équipe', -1, 9, 30, 30, { location: 'Salle Lumière' }),
    ...[0, 1, 2, 3].map(d => event(4, 'Stand-up', d, 9, 30, 15, { location: 'https://meet.google.com/abc-defg-hij' })),
    event(5, 'Atelier marque — Maison Brune', 0, 10, 0, 90, {
      location: 'https://meet.google.com/xyz-mbru-nne', organizer: 'Sophie Andrieu <sophie.andrieu@maison-brune.example>',
      description: 'Ordre du jour :\n– retours sur les pistes typographiques\n– palette automne\n– calendrier des livrables\n\nSupports : https://nimbus.example/d/maison-brune-pistes'
    }),
    event(4, 'Revue maquettes Lumen', 0, 11, 0, 60, { location: 'Salle Lumière', status: 'tentative' }),
    event(4, 'Appel imprimeur', 0, 14, 0, 45),
    event(4, '1:1 Nora', 1, 9, 0, 30),
    event(5, 'Présentation identité visuelle Lumen', 1, 14, 0, 120, {
      location: 'Lumen, 12 rue de la République, Lyon', organizer: 'Camille Laurent <camille@atelier-nova.example>',
      meeting: 'https://meet.google.com/lum-enid-ntt', reminders: [30],
      description: 'Présentation des trois pistes d\'identité, puis choix de la direction.\nSupports : https://nimbus.example/d/lumen-identite',
      attendees: [
        { email: 'camille@atelier-nova.example', name: 'Camille Laurent', response: 'accepted', organizer: true },
        { email: 'claire@lumen.example', name: 'Claire Fontaine', response: 'accepted', organizer: false },
        { email: 'thomas@lumen.example', name: 'Thomas Ricard', response: 'tentative', organizer: false },
        { email: 'julien.mercier@atelier-nova.example', name: 'Julien Mercier', response: 'accepted', organizer: false },
        { email: 'nora.benali@atelier-nova.example', name: 'Nora Benali', response: 'needs_action', organizer: false }
      ]
    }),
    event(4, 'Shooting produit', 2, 10, 0, 150, { location: 'Studio Nova' }),
    allDay(4, 'Salon Graphisme Lyon', 2, 2, { location: 'Halle Tony Garnier' }),
    event(4, 'Rétro de sprint', 3, 16, 0, 60, { status: 'tentative' }),
    event(5, 'Livraison maquettes Maison Brune', 8, 11, 0, 60),
    event(4, 'Déjeuner d\'équipe', 9, 12, 30, 90, { location: 'Le Comptoir' }),
    // Les Roues Libres
    event(7, 'Réunion du bureau', 1, 18, 0, 60, { location: 'Local du club' }),
    event(7, 'Atelier réparation', 2, 18, 30, 90, { location: 'Local du club', description: 'Crevaisons, câbles de frein : venez avec votre vélo.' }),
    event(7, 'Sortie vélo — Monts d\'Or (65 km)', 4, 8, 30, 240, { location: 'Parking de la piscine de Vaise', organizer: 'Inès Garnier <ines.garnier@example.net>' }),
    event(7, 'Sortie vélo — Val de Saône', 11, 8, 30, 210),
    event(7, 'Assemblée générale', 25, 18, 30, 120, { location: 'Salle des fêtes de Valfleury' })
  ].sort((a, b) => a.start - b.start)

  // ── Other screens ─────────────────────────────────────────────────────────

  const followUps = [
    { id: 1, account_id: 2, subject: 'Proposition — identité visuelle Lumen', to: ['Claire Fontaine'], sent_at: at(6, 11, 20), due: at(1, 11, 20), thread_id: null, mailbox_id: null }
  ]

  const pinnedArchive = { account: 1, subject: 'Attestation d\'assurance habitation 2026', from: p.frequence, date: at(30, 10, 5) }

  const contacts = [p.ines, p.julien, p.sophie, p.nora, p.thomas, p.marc, p.helene, p.lea, p.antoine]

  const months = ['2025-11', '2025-12', '2026-01', '2026-02', '2026-03', '2026-04', '2026-05', '2026-06', '2026-07', '2026-08', '2026-09', '2026-10']
  const received = [412, 388, 455, 431, 502, 476, 498, 523, 361, 298, 547, 214]
  const sent = [96, 84, 112, 101, 128, 117, 122, 131, 74, 52, 139, 58]

  window.__FACTEUR_DEMO__ = {
    NOW, MIN, HOUR, DAY, at, me, accounts, people: p, logos, threads, held, followUps, pinnedArchive, contacts,
    stats: { months, received, sent },
    calendar: { calendars, status: calendarStatus, events }
  }
})()
