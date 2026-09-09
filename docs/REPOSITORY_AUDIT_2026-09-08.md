# Repository-Audit und UI-Korrekturen — 2026-09-08

## Arbeitsstand und Umfang

Geprüft wurde `openaircast/main` auf Basis von `9309f80` im Worktree
`.worktrees/gui-control-center-implementation`. Der übergeordnete Checkout
`feat/gui-control-center` ist nicht der aktuelle Implementierungsstand.
Änderungen dieses Audits wurden als `a0df2ee` committed und auf
`origin/openaircast/main` gepusht. Der GitHub-Standardbranch `master` wurde
nicht verändert. Ein frischer vollständiger Testlauf vor dem Commit bestätigte
erneut 1.969 bestandene und 6 ignorierte Tests.

Prüfung: unabhängige Backend-/UI-Quellcodeaudits, gezielte RED/GREEN-Regressionen,
vollständige Offline-Workspace-Tests und Clippy. Kein umfassendes Security-Audit,
kein Hardware-Langzeittest und keine Aussage über sämtliche möglichen Fehler.

## Behoben

- Audioquelle, Mute und Latenz zeigten nach Backend-Ablehnung teilweise den
  angeforderten statt den tatsächlich bestätigten Zustand. Der Reducer wartet
  jetzt auf Bestätigung; ein unveränderter Backend-Snapshot hinterlässt keinen
  falschen UI-Wert und erzeugt keine Projektionsschleife.
- Nach erfolgreicher Wiederverbindung blieb die alte Session-Fehlermeldung
  sichtbar. Passende Erfolgsmeldungen entfernen jetzt ausschließlich diesen
  Fehler; fremde Hinweise und veraltete Generationen bleiben korrekt behandelt.
- Empfänger- und Switch-Fokusringe wurden am eigenen Widget-Rechteck abgeschnitten.
  Der äußere Ring bleibt jetzt sichtbar innerhalb des übergeordneten Clips.
- Empfängerkarten übernahmen beim manuellen Textlayout die spezifizierten
  Zeilenhöhen nicht. Umgebrochener Text erhält nun passende Höhe und Kartenfläche.
- Nach Änderung der Audioendpunkt-Reihenfolge konnte die Tastatur den falschen
  Endpunkt auswählen. Fokusidentität folgt nun dem opaken Geräteschlüssel.
- Workspace-Repository-Metadaten verweisen auf `TTK95/OpenAirCast` statt Example-URL.

## UI und Optimierung

- Markenbereich in der Navigation, vereinheitlichte Vektorsymbole und klare
  Unterscheidung aktiver/inaktiver Einträge.
- Runder Lautstärkegriff und eingefärbter eingestellter Schienenanteil.
- Vorhandene Windows-Native-Palette, Layout-Breakpoints, Übersetzungen,
  High Contrast und Zustandsarchitektur bleiben erhalten.
- Accessibility-Matrix verwendet Testfenster pro Theme/Locale wieder, statt
  Fonts und Fenster für jeden einzelnen Zustand neu aufzubauen. Die vollständige
  Kombination von Ansichten, Größen, Sprachen und Zuständen bleibt geprüft.
- Detaillierte Fortschreibung: [Windows-Native-Anleitung](UI_WINDOWS_NATIVE_2026-09-08.md).

## Verifikation

- `cargo test --offline --workspace --all-targets --no-fail-fast --quiet`:
  Exit 0, **1.969 bestanden, 6 ignoriert, 0 fehlgeschlagen**. Ignorierte Tests
  sind keine bestandenen Hardwareprüfungen. Benchmark-Testausführungen ebenfalls
  erfolgreich; daraus wird kein Durchsatzbenchmark abgeleitet.
- GUI-Binary-Tests: **655 bestanden, 2 ignoriert**, 51,19 Sekunden für diese Suite.
  Vor der Optimierung wurde ein Baseline-Lauf beim lang laufenden
  Accessibility-Matrixtest nach mehreren Minuten gezielt abgebrochen.
- `cargo clippy --offline --workspace --all-targets`: Exit 0, weiterhin
  Bestandswarnungen (unter anderem ungenutzter Code, deprecated APIs,
  vereinfachbare Ausdrücke und große Enum-Varianten). Kein warnungsfreier Build.
- `cargo build --offline -p homepod-cast --bin openaircast`: Exit 0. Aktuelle
  ausführbare Datei: `target/debug/openaircast.exe` im Implementierungs-Worktree.
- Mehrere unabhängige Reviews; keine neuen wichtigen/kritischen Befunde in den
  geprüften Korrekturen. Der ursprüngliche Lifecycle-Befund wurde in der
  anschließenden, unten dokumentierten Folgerunde bearbeitet.

## Folgerunde: Busy-Queue im aktiven Backend

- Start reserviert vor dem ersten Enqueue alle drei benötigten Queue-Plätze.
  Bei `Busy` gelangen weder Membership noch Lautstärke noch Run Intent in die
  Queue. Dies garantiert gemeinsame Zulassung, keine atomare Backend-Transaktion.
- Bei abgewiesenem Start oder Stop markiert die Bridge dieselbe Shell-Generation
  als abgewiesen und projiziert den unveränderten, tatsächlichen Backend-Zustand
  erneut. Laufendes Audio wird nicht als gestoppt gemeldet; aktive Receiver
  werden wiederhergestellt. Die Shell-Auswahl bleibt Shell-eigener Zustand.
- Generation und Zulassungsergebnis werden zusammen unter einem kurzen Mutex
  gelesen. Kein Lock bleibt über `await` gehalten; keine Generation wird
  zurückgesetzt. Vor Abschluss der Zulassung wird kein neues Ergebnis geweckt.
- Die vorhandene begrenzte Outbox hält Rückmeldungen bei voller UI-Queue zurück
  und bietet sie erneut an. Veraltete Generationen verändern keine neuere Session.
- RED belegte hängendes Starting/Stopping und teilweise Start-Zulassung.
  GREEN: 102 Bridge-Tests und 7 Command-Tests. Sieben neue Regressionstests
  prüfen Queue-Kapazitäten, Membership, verzögerte Rückmeldung, veraltete
  Generationen und echten Threadbetrieb mit einem einzigen Rückmeldeplatz.
- Begrenzung: Kein eigener Busy-Hinweis hinzugefügt. Die wahrheitsgemäßen
  Start/Stop-Aktionen werden wieder bedienbar. Der nicht aktive Legacy-Controller
  wurde nicht geändert; bei einer späteren Umschaltung muss dessen Busy-Verhalten
  separat abgesichert werden.
- Abschließender vollständiger Workspace-Testlauf: **1.976 bestanden,
  6 ignoriert, 0 fehlgeschlagen**, Exit 0; GUI-Binary-Suite 661 bestanden,
  2 ignoriert. Normaler Debug-Build ebenfalls Exit 0. Clippy für alle
  Workspace-Targets Exit 0 mit Bestandswarnungen; rustfmt und Diff-Check sauber.
  Ein voriger Versuch wurde durch die laufende, selbst gestartete Test-EXE
  gesperrt (`os error 5`). Nach Beenden ausschließlich dieses Testprozesses
  wurden vollständige Tests und Build erneut erfolgreich ausgeführt.

## Offen — priorisierte nächste Schritte

1. **Busy-Queue: Bedienrückmeldung ergänzen, falls gewünscht.** Der ursprünglich
   priorisierte Hänger ist im aktiven Backend durch die Folgerunde behoben.
   Ein eigener lokalisierter Hinweis auf abgewiesene Befehle ist noch nicht
   implementiert. Nicht dafür `SessionFailed` oder `ControllerUnavailable`
   zweckentfremden: eine volle Queue ist kein ausgefallener Audiocontroller.
2. **Native visuelle Abnahme.** Die Computersteuerung lehnte den App-Start wegen
   fehlender App-Freigabe zunächst ab. Nach dem Neustart der Host-App mit den vom
   Nutzer bereitgestellten Rechten gelang der native Start am selben Tag.
   Die Fensteraufnahme scheitert jedoch auch nach erneuter Fensterauswahl mit
   `SetIsBorderRequired failed: Schnittstelle nicht unterstützt (0x80004002)`.
   Die reine Accessibility-Abfrage liefert nur den Fensterrahmen, keine
   App-Inhalte. Deshalb weiterhin keine Screenshots oder visuelle Designfreigabe;
   der gestartete native Prozess und das Fenster sind bestätigt.
   Nach Freigabe Hell/Dunkel/High Contrast, DE/EN, 900×600/1120×720 und
   Windows-Skalierung 100/150 % sowie echte Tray-Bedienung prüfen.
3. **Hardware-Release-Gates.** Audioquellenwechsel, Receiver-Ausfall/Reconnect,
   Start/Stop, Mehrraum-Synchronität und längerer Stream auf realen Geräten.
4. **Bestehendes Produkt-Backlog abgleichen.** Capture-Endpoint-WIP `469b530`,
   Saved Groups, Per-Receiver-Steuerung und gPTP Peer Delay nicht allein anhand
   historischer Handoff-Checkboxen für abgeschlossen erklären. Diese Runde
   implementiert keine neuen Gruppen- oder PTP-Funktionen.
5. **Lint-Schulden separat abbauen.** Keine pauschalen automatischen Fixes am
   Protokoll-/Kryptocode; dort zuerst Spezifikation und Regressionen prüfen.

## Sichere Fortsetzung

Im genannten Worktree beginnen, `git status --short` lesen und die lokalen
Auditänderungen erhalten. Zuerst offene native Release-Gates und den tatsächlichen
Capture-Endpoint-WIP-Stand prüfen. Weitere Änderungen mit TDD, unabhängiger
Review und Workspace-Tests. Keine Protokolländerung ohne `AIRPLAY_2_SPEC.md`.
Nicht automatisch committen, pushen oder verworfene WIP-Branches integrieren.
