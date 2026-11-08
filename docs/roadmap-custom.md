# Wenyi Custom Roadmap — Backend bleibt Python, gezielt erweitert

Stand: 2026-11-05. Prinzip: kleinschrittig, jeder Schritt einzeln verifiziert,
nichts Bestehendes kaputt machen. Backend bleibt Python; Rust-UI nur dort
anfassen, wo Anzeige/Interaktion es erfordert.

Zielbild (05.11.): 3 Translate-Worker parallel an verschiedenen Kapiteln
(Cloud-Modelle) + 1 Glossar-Curator lokal. Dafür ist Schritt 4 das Fundament.

Stand 05.11. abends: Schritte 0–4 ✅, Provider live (`local`/llamacpp mit
`hy-mt2-7b`/`local_27b`/`local_9b` in deinen Settings angelegt und gepingt),
Worker-Polish ✅ (Resume pro Kapitel, Lane-Helper), Schritt 6 ausdesignt
(nächster Fokus). Deine App läuft mit neuem Code — Neustart holt den
aktuellen Release-Stand (t/s-Karte, Settings-Grid).

Stand 05.11. nachts: echte Output-Rate ✅ (`completion_ms` pro Call,
`completion_tokens/ms` statt Total/Zeit), Backend-Polish ✅ (Manifest-Cache
in load_chapter, Kapitel-Filter in Events, Issue-Counter, Regex-Cache,
1× list_jobs), Project-Settings im Global-Stil ✅ (Tabellen-Zeilen +
Gruppen), Curator-Backend ✅ (Job-Art, Op, Lock, Scheduler, Routen) +
Worker-Tab ✅ (Status, Start/Stopp, Historie). Suite 2002, Rust 23.

Stand 06.11.: Run-History ✅ (Scrollbox + Show more, Layout stabil),
Worker-Live-Poll ✅ (3s bei laufendem Pass), Hover ✅ (Wash statt Vollfläche,
Card-Outline), Sidebar ✅ (Persist-Debounce), Curator-Feed ✅ (Activity +
Current-Term + Output-Cap), Glossar-Tabelle ✅ (mentions-Spalte, Tags,
Aktionen rechts), Style-Priorität ✅ (MANDATORY-Regel). Suite 2004, Rust 23.
Offen: Websuche (Stub), Style-Update-Loop (Schritt 7), Template-ID-Migration.

---

## Schritt 0 — Diagnose: Translate/Resume tut nichts ✅ (04.11.2026)

**Root Cause: Das Modell hinter `beam-free` wurde vom Gateway ausgetauscht
und ist für translation.body unbrauchbar.**

- Job 24 starb mit `empty_response` (5× leerer Content bei HTTP 200).
- Live-Repro (Resume 04.11. 22:17): `beam-free` loopt stattdessen —
  47k-Prompt → exakt 32.768 Completion-Tokens → `TruncatedResponseError`,
  2× reproduziert, je ~160s. Bei 1.800-Token-Batches sind 32k Output
  kein legitimes Ergebnis, sondern ein Generierungs-Loop.
- ⚠️ Korrektur (Agenten-Review): `inference_fingerprint` ist ein LOKALER
  Config-Hash (`routing.py:80-107`), kein Gateway-Fingerprint — die Änderung
  `3b49…→5563…` beweist KEINEN Modelltausch, sondern eine lokale
  Config-Änderung (Projekt-Overrides!). Ob das Gateway-Modell gleich ist,
  bleibt offen. Führende Hypothese: Reasoning-Variante hinter dem Alias,
  deren Text in `reasoning_content` landet, wo Wenyi nie hinschaut
  (`_openai_compatible.py:157-170` liest nur `message.content`).
  Entscheidender Test: Raw-Response-Dump per Ping-Prompt (siehe Anhang).
- Ping mit „Reply OK" antwortet ebenfalls leer (`category: reachable`,
  5 von 8 Profilen betroffen — gateway- oder modellspezifisch).
- Kein Config-/Code-Fehler: Batch-Planung (1.800 Tokens), Prompt-Aufbau
  (Batch + volles Glossar + Style/Synopsis), Retry-Logik arbeiten korrekt.
- Zusatzfund: **Projekt-Overrides schlagen globale Tiers.** Dein Projekt
  pinnt `strong/cheap/fast = beam-free` (`config_overrides` im Projekt-
  Dokument); die globale Umstellung auf `muse-spark-13-contributor` greift
  deshalb nicht für dieses Projekt. Resume baut den Snapshot aus
  `effective_config` = global + Projekt-Override (`job_service.py:42`).

**Fix (kein Code, nur Config):** In den Project Settings (Screenshot
04.11.) die Tiers auf ein funktionierendes Modell stellen —
`muse-spark-13-contributor` oder `kilo_ultra` (einziges Profil mit
sauberem Ping `ok`). Danach Resume → Job nutzt den neuen Snapshot.
`beam-free` vorerst nirgends als `strong` verwenden.

## Schritt 1 — Custom Local-Endpoint-Provider ✅ (05.11.2026)

**Ergebnis:** Provider-Klasse `llamacpp` (`llm/providers/llamacpp.py` +
Registry-Eintrag) für das llama.cpp-Gateway auf `:8093` steht; Ping/List
laufen, alle 4 Gateway-Modelle sichtbar. `hy-mt2-7b` (Hy-MT2-Q4_K_S aus
LM Studio) ist als `hymt7b` ins Gateway eingebunden
(`router-models.ini`) und übersetzt korrekt.
Modell-Feintuning für 170 t/s (Kontext 10–30k statt 100k) ist geparkt —
Plumbing zuerst, Speed später.

**Ungeplant, aber nötig (✅): Prompt-Diät.**
Anatomie: 47k-Prompt = 43k Glossar (3.020 Terme, 91%) + Rest. Fix:
`BatchPlan.capture` bekommt nur noch batch-relevante Terme
(`GlossaryStore.terms_in`, existierende Wortgrenzen-Funktion) —
`translation.py:321`. Effekt an echten Daten: 3.020 → ~100 Terme,
Prompt 47k → ~6,7k (86% weniger). E2E bewiesen: `hymt7b` übersetzt
Kapitel 13 lokal (18/31 Segmente verifiziert, kein Fehler).
Tests: 2 Test-Dateien auf Filter-Verhalten umgeschrieben, Suite grün.
Nebeneffekt: Cloud-Kosten pro Batch sinken ebenfalls ~85%.

**Offen (nicht blockierend):** `load_translation_inputs` braucht ~8 Min
über die 122MB-DB vor dem ersten Batch — nächster Flaschenhals, später.

## Schritt 2 — Avg Tokens/Sekunde auf der Overview ✅ (05.11.2026)

**Ziel:** Unter „Total usage & run time" zusätzlich Ø Tokens/s anzeigen.

**Daten vorhanden, keine Pipeline-Änderung nötig:**
- `usage.json → totals.total_tokens` (Schema in `llm/usage.py`)
- `timing.json → total_seconds` (Schema in `timing.py`)
- Formel: `total_tokens / total_seconds` (Nenner 0 → „—").

**Arbeit:**
1. Rust: `progress.rs` Overview — fünfte Kennzahl aus `snapshot.stats`
   (`usage.totals.total_tokens / timing.total_seconds`), Format `1,234 t/s`.
2. Optional Python: `GET /stats` liefert `derived: {tokens_per_second}` mit,
   damit Web-UI denselben Wert nutzt (eine Stelle, keine Doppel-Logik).

**Fertig wenn:** Zahl steht auf der Overview, stimmt mit Tokens/Zeit überein,
„—" bei 0 Sekunden, kein Layout-Bruch.

## Schritt 3 — Project-Settings-Seite aufräumen ✅ (05.11.2026)

**Ziel:** Die ~16 Operation-Model-Dropdowns (Screenshot 04.11.) nicht mehr
vollständig untereinander, sondern kompakt — z.B. gruppiert nach Phase
(Analyse / Translation / Glossary / Review / Autofix) in zwei Spalten.

**Arbeit (nur Rust, `proj_settings.rs` + ggf. Widgets):**
1. Operationen in 4–5 Gruppen einteilen (Mapping in einer Tabelle, kein
   Backend nötig — Keys bleiben identisch).
2. Zweispaltiges Grid pro Gruppe; Tier-Dropdowns unverändert.
3. Screenshot-Verifikation aller Gruppen, Speichern/Laden unverändert testen.

**Fertig wenn:** Seite passt ohne endloses Scrollen auf einen Blick;
alle Werte laden/speichern wie vorher (kein Backend-Change).

## Schritt 4 — Multi-Worker: 3× Translate + 1× Glossary parallel (jetzt)

**Zielbild:** 3 Translate-Worker an verschiedenen Kapiteln (Cloud) +
1 Glossar-Curator (lokal), alle am selben Projekt, ohne 409er und ohne
verlorene Updates.

**Heute (3 Blockaden):**
1. `project_write` (`project_service.py:47`): `.run.lock` non-blocking +
   `busy()` → 409 bei jedem Zweit-Job.
2. `admit_job` (`local_backend.py:367`): Projekt-Status in
   `RUNNING_PROJECT_STATUSES` → 409.
3. Worker hält `.run.lock` für den gesamten Run (`_run_locked`).
Dazu semantisch: `usage.json`-Merge ist Read+Write ohne Schutz (lost
updates bei 2 Schreibern); `term_snapshot` veraltet pro Worker (ok —
Konflikte löst der Curator); Kapitel-Keys sind disjunkt (kein Konflikt).

**Arbeit (kleinschrittig, je einzeln verifiziert):**
1. **4a — Usage-Merge sichern ✅ (05.11.):** `flush_usage` läuft unter
   `state_lock` (`runtime.py:113`); Race-Test (8 Worker × 5 Runden)
   beweist: ohne Fix 18/40 Tokens verloren, mit Fix 40/40 erhalten.
2. **4b — Kapitel-Locks ✅ (05.11.):** `chapter_lock(ci)` im Protokoll +
   sqlite/runstore/file/pg; `admit_job` mit Kapitel-Regel (atomar);
   `project_write(chapter=…)` ohne `.run.lock`-Probe (maßgeblich ist die
   Job-Tabelle); Worker hält `.chapter-N.lock`; `_record_terminal_status`
   setzt Projekt-Status nur ohne aktive Geschwister; `_execute`-Prelude
   pro Lane (kein globales Latest-wins); 3 Workflow-Consumer + Shutdown
   mit Konstanten. Tests auf 3 Lanes umgeschrieben.
3. **4c — Job-Status pro Kapitel ✅ (05.11.):** `running_chapters` in
   `GET /workflow` (aus aktiven Jobs, kein neues Feld) + Rust-Feld
   `WorkflowOut.running_chapters`.
4. **4d — Curator-Ausnahme:** Job-Art `glossary_curator` (Schritt 6)
   braucht keinen Kapitel-Lock (schreibt nur Glossar-Tabellen),
   max. 1 pro Projekt (eigener `.curator.lock`). — Offen bis Schritt 6.
5. Verifizieren ✅ (05.11.): 3 Kapitel-Jobs parallel angenommen (kein
   409), Duplikat → 409 „conflicting", Buch-Job bei aktivem Kapitel →
   409, Jobs liefen überlappend; Suite 1996 grün, Ruff clean.

**Fertig wenn:** 3 Kapitel + Curator laufen parallel; Usage stimmt;
Regel in `docs/architecture.md`. Erst danach Schritt 6.

## Schritt 5 — Glossary Curator v1: Dubletten + Kasus (ohne LLM-Agent)

**Ziel:** Die zwei konkret genannten Schmerzen lösen, bevor ein Agent kommt:
(a) Dubletten („zweimal derselbe Begriff, etwas anders"),
(b) Kasus-Formen (Genitiv eingetragen statt Nominativ).

**Warum zuerst ohne Agent:** Deterministisch, testbar, sofort nützlich —
und liefert dem späteren Agenten saubere Grunddaten.

**Arbeit:**
1. **Normalisierung beim Eintragen** (`glossary/store.py:309`
   `upsert_term`): deutsche Kasus-Endungen auf Grundform mappen
   (einfache Suffix-Regeln für Eigennamen: -s/-es/-n/-en/-m/-r + Artikel-
   Erkennung), plus Case-/Whitespace-Normalisierung für den
   Dublet-ten-Vergleich. Originalform bleibt in `aliases` erhalten.
2. **Dublet-ten-Report** (neuer Endpoint `GET /glossary/duplicates`):
   gruppiert Terme nach normalisierter Form, zeigt Kandidaten mit Kapiteln.
3. **UI:** Glossar-Seite zeigt Dublet-ten-Gruppe mit „Zusammenführen"-
   Aktion (ein `POST /glossary/merge`, löscht Verlierer, hängt Aliase an).
4. **Kontext-Spalte:** Zu jedem Term die Erstfundstelle
   (`first_chapter` existiert; zusätzlich Segment-Anchor + Satz-Ausschnitt
   aus `source_corpus` — kein neues Grab-Tool nötig, die Daten liegen in
   `translation.py:123 load_translation_inputs` bereits vor).

**Fertig wenn:** Genitiv-Einträge landen als Nominativ; Dublet-ten-Liste
zeigt echte Paare; Merge funktioniert; Kontext-Satz steht beim Term.
Tests für Normalisierung + Merge.

## Schritt 6 — Glossary Curator: Background-Worker mit eigenem LLM (nächster Fokus)

**Zielbild (05.11.):** Unsichtbarer Hintergrund-Worker, der permanent das
Glossar poliert — pro Term: Vorkommen im Buch finden (Buch-Grep) →
Reasoning mit eigenem, zuweisbarem LLM → ggf. Websuche (z.B. Band 1 auf
Deutsch existiert, Band 2–8 nicht) → Vorschlag als Konflikt (Mensch
entscheidet). Alle paar Minuten ein Durchgang, kein stilles Überschreiben.
Sprach-agnostisch gebaut, Deutsch zuerst verifiziert.

**Baut auf:** Schritt 4 (parallele Jobs, `.curator.lock` aus 4d) +
Schritt 5 (Normalisierung als Matching-Basis — ggf. auf das Nötigste
reduziert: der Curator normalisiert selbst im Prompt).

**Workflow pro Durchgang (alle 5 Min, ein Kapitel-Fenster pro Lauf):**

```text
1. CANDIDATES  — Nächste ~20 Terme aus der Curator-Queue
                  (Priorität: Konflikte > frisch extrahiert > lange nicht
                  geprüft; Queue in `glossary` als `curator_checked_at`).
2. BOOK-GREP   — `find_occurrences(term)`: alle (Kapitel, Segment)-Stellen
                  aus `source_corpus` + aktuelle Targets aus `history`
                  (beide aus `load_translation_inputs`, kein neues Tool).
                  Max. 5 Belegstellen pro Term (erste, letzte, 3 Mitte).
3. REASON      — EIN LLM-Call `glossary.curate` pro Term (eigenes Modell,
                  Profil pro Projekt zuweisbar, Default: lokales Modell):
                  Grundform korrekt? Übersetzung konsistent über Belege?
                  Dublette von Term X? Antwort als strukturiertes JSON.
4. WEB (nur bei Bedarf) — Wenn Belege widersprechen UND Buch-Kontext
                  nicht reicht: 1 Suche „<Buchtitel> <Term> deutsch",
                  Top-3-Snippets cachen (`curator_web_cache`-Tabelle,
                  7 Tage TTL), als Evidenz in den Konflikt-Vorschlag.
                  Generisch (kein Deutsch-Sonderpfad): Query aus
                  source/target-Lang + Buchtitel gebaut.
5. PROPOSE     — Ergebnis NIE direkt schreiben: als `term_conflicts`-
                  Eintrag (`note` = Reasoning-Kurzform + Belegstellen)
                  oder `verified` (kein Handlungsbedarf). Event-Log:
                  `curator_term_checked` pro Term (leise, kein Spam:
                  nur Summary pro Durchgang + Konflikte einzeln).
```

**Bausteine (Nähte existieren):**
1. **Job-Art `glossary_curator`** in `job_service.py` + Branch in
   `tasks.py` + Route. Schreibt nur `glossary`/`term_conflicts`/
   `curator_*`-Keys (Regel aus Schritt 4), hält `.curator.lock`
   (max. 1 pro Projekt), kein Kapitel-Lock.
2. **Scheduler:** Maintenance-Loop in `local_runtime.py`
   (Vorbild `_recover_periodically`, 30s-Takt): startet den Curator-Job,
   wenn keiner läuft und die Queue nicht leer ist. Intervall pro
   Projekt konfigurierbar (Default 5 Min), global abschaltbar.
3. **Eigenes LLM:** Neue Operation `glossary.curate` in `operations.py`
   (Tier-Default `cheap`) + Zuweisung in Project Settings
   (Schritt-3-Grid bekommt die Op automatisch). Damit läuft z.B.
   `local_hymt` nur fürs Glossar, Cloud für Body.
4. **`find_occurrences(term)`** in `glossary/extractor.py`: exakt +
   normalisiert (Schritt-5-Regeln wiederverwenden), über
   `source_corpus`/`history`.
5. **Websuche:** `httpx`-Client (Dep schon da) gegen eine konfigurierbare
   Search-API (Default: DuckDuckGo-HTML, kein Key; später optional
   bezahlte API). Strikt begrenzt: 1 Query, 3 Snippets, Cache, Timeout
   10s, Fehler → still überspringen (Buch-Evidenz reicht meist).
6. **UI (minimal):** Glossar-Seite: Badge „Curator: N offen",
   Konflikt-Zeilen zeigen Belegstellen; Overview: nichts (Hintergrund).

**Fertig wenn:** Curator läuft 1h parallel zur Übersetzung ohne 409;
Dubletten/Kasus-Fälle aus deinen Daten erscheinen als Konflikte mit
Belegstellen; Web-Evidenz bei mind. 1 Term belegt; abschaltbar;
Suite grün.

## Schritt 7 — Style-Bereich lebendig (nach Curator v1)

**Ziel:** Style-Analyse wird laufend aktualisiert, darf ausführlich sein,
bleibt auf den Punkt.

**Heute:** `analysis.style` läuft einmal in Prepare (`analyzer.py`),
Ergebnis liegt als Artifact — danach statisch.

**Arbeit:**
1. Style-Artifact nach jedem fertigen Kapitel anhängen/aktualisieren
   (neue Operation `analysis.style_update`, billiges Modell, nur Diff).
2. Style-Seite zeigt Versionen (pro Kapitel ein Stand, aufklappbar).
3. Länge: kein hartes Limit — Prompt verlangt Struktur
   (Ton/Register/Satzbau/Anrede/Stolpersteine), keine Stichwort-Kürze.

**Fertig wenn:** Nach Kapitel N steht Style-Stand N bereit; alte Stände
einsehbar; Übersetzung nutzt jeweils den aktuellen Stand.

---

## Nicht-Ziele (bewusst ausgeklammert)

- Kein Rust-Rewrite des Backends.
- Kein freier Web-Browsing-Agent (nur gezielte Suche in Schritt 6.4).
- Kein stilles Auto-Überschreiben von Glossar/Übersetzung durch Agenten —
  Vorschläge als Konflikte, Mensch entscheidet.
- Kein Redesign von Event-Log/Interface-Settings (bereits gut).
