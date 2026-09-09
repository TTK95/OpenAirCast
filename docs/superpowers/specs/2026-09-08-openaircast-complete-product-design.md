# OpenAirCast — Gesamtplan bis Version 1.0

> **Freigabeentscheidung 2026-09-09:** Commit, Merge, Push und
> Release-Vorbereitung sind trotz des bekannten Standby-/Abbruchfehlers vom
> Eigentümer ausdrücklich freigegeben. Frühere Integrationssperren unten
> dokumentieren den technischen Reviewbefund vor dieser Risikoakzeptanz.
> Der Fehler bleibt offen; ausstehende 1.0-Prüfungen bleiben ausstehend.
> Siehe [Releasehinweise](../../RELEASE_NOTES_2026-09-09.md).

**Stand:** 2026-09-09, Umsetzung auf `codex/five-point-completion`;
aktueller Nachweis und Commitstand in [RELEASE_CHECK_2026-09-09.md](../../RELEASE_CHECK_2026-09-09.md).
**Status:** Vom Nutzer zur Umsetzung freigegeben am 2026-09-08.
**Aktuelle Priorität:** Die Mehrraum-Regression ist durch die bestätigte
Wiedergabe auf allen drei Empfängern und funktionierende Einzelpegel behoben.
Die Pegelanwendung vor dem ersten Audiopaket ist inzwischen im produktiven
Transportpfad implementiert und deterministisch geprüft. Frische Hardware-
Langzeit-/Skew-Messungen und physische Start-/Rejoin-Prüfungen bleiben offen;
P6-Präzisionsarbeit folgt danach.

Dieses Dokument verbindet die bisherigen Shell-, Resilienz-, Diagnose- und
Windows-Native-Pläne zu einem gemeinsamen Fertigstellungsziel. Es ist kein
pauschaler Auftrag, alle Teilprojekte sofort umzusetzen. Nach Freigabe erhält
jedes Teilprojekt einen eigenen ausführbaren TDD-Plan mit konkreten Tests,
Schnittstellen und Review-Checkpoint. Die vorhandenen älteren Codepläne dürfen
nicht ungeprüft ausgeführt werden: Einige dort neu anzulegenden Komponenten
existieren inzwischen unter anderen Pfaden.

## 1. Produktentscheidung

**Empfohlener Weg:** Die bestehende Rust/egui-App zur vollständigen Version 1
ausbauen. Funktionierende Transport-, Backend- und UI-Grundlagen weiterverwenden;
Bedienabläufe und fehlende Ende-zu-Ende-Verbindungen gezielt fertigstellen.

Alternativen:

- Nur Oberflächenpolitur: geringerer Aufwand, aber Gruppen, Einzelpegel und
  Supportfunktionen bleiben unvollständig. Erfüllt das Produktziel nicht.
- Wechsel auf WinUI: stärkeres Framework-Nativverhalten, aber neue Abhängigkeiten,
  Integrationsarbeit und erneute UI-Testbasis. Kein nachgewiesener Nutzen für
  die aktuellen Funktionslücken; deshalb nicht Teil dieses Plans.

**Produktversprechen:** Windows-Systemaudio einmal aufnehmen und an einen oder
mehrere kompatible AirPlay-2-Lautsprecher übertragen. Auswahl, Wiedergabe,
Lautstärke und Fehlerbehebung sind ohne technische Vorkenntnisse bedienbar.
Eine Diagnoseebene erklärt tatsächlich gemessene Zustände.

## 2. Was „fertig“ bedeutet

Version 1.0 ist erst fertig, wenn alle Pflicht-Teilprojekte und Release-Gates
abgenommen sind. Ein erfolgreicher Build, eine vorhandene Seite oder eine
Backend-Command-Variante allein bedeutet nicht „Feature fertig“.

### Pflichtumfang 1.0

- Windows 10/11, x64; normaler Betrieb ohne Administratorrechte und ohne Konsole.
- HomePod/HomePod mini als zunächst zu validierende Gerätefamilie; andere
  AirPlay-2-Geräte nur mit klar ausgewiesener, tatsächlich geprüfter Unterstützung.
- Ein gemeinsamer WASAPI-Capture-/Encode-Pfad; keine Aufnahme je Lautsprecher.
- Einzel- und Mehrraumwiedergabe, Start/Stop, Masterpegel, globale Stummschaltung,
  Auswahl und Wechsel der Windows-Audioquelle, tatsächlich unterstützte Latenzen.
- Lautsprecherauswahl auf Übersicht und Lautsprecherseite; Einzelpegel und
  gezieltes Wiederverbinden, soweit die Backend-Capability vorhanden ist.
- Gespeicherte Gruppen erstellen, ändern, löschen und übernehmen; explizites
  „Übernehmen und starten“ statt unbeabsichtigtem Audiostart beim Bearbeiten.
- Letzte Auswahl dauerhaft speichern; optional automatisch verbinden;
  optional mit Windows starten; Tray, Hotkey, sauberes Beenden.
- Sinnvolle Diagnose auch ohne Stream, Details je Empfänger, Ereignisse,
  sichere Zusammenfassung/Export und vorhandene manuelle Kalibrierung.
- Deutsch/Englisch, Hell/Dunkel/System, Windows High Contrast, Tastatur und
  Screenreader, 900×600 bis größere Fenster, 100–200 % Skalierung.
- Portable Release-EXE mit Quellcodebezug, Prüfsumme, Lizenzen, Versions- und
  Installationsanleitung. Keine Cloud, kein Konto, keine Telemetrie.

### Bewusst nach 1.0 oder an Bedingungen gebunden

- Installer und automatische Updateinstallation: eigener nachgelagerter Ausbau;
  1.0 wird vollständig als portable App bedienbar und aktualisierbar geliefert.
- Codesignierung: nur mit vorhandener Infrastruktur; niemals Zertifikat oder
  Publisher-Vertrauen vortäuschen. Fehlende Signierung transparent dokumentieren.
- Apple-TV-PIN-Pairing: Fähigkeit zuerst prüfen. Nur bei vollständigem sicheren
  Workflow freischalten; andernfalls als nicht unterstützten Gerätemodus nennen,
  nicht als funktionierendes Feature darstellen.
- Vollständiges gPTP Peer Delay: Präzisionsarbeit nach den Produktfunktionen.
  Wird zum Release-Blocker, falls die Synchronitäts-Gates mit dem vorhandenen
  Verfahren nicht bestehen. Kein ungeprüftes Übernehmen von `998f817`.
- Automatische akustische Kalibrierung, per-Gerät-DSP, lokale Gerätealiasnamen,
  nahtloses Hot-Join ohne Neuverhandlung und zusätzliche Plattformen: nicht 1.0.
- Ausgeschlossen: Video, Bildschirmspiegelung, AirPlay-Empfang, DRM, Bluetooth,
  Internet-Streaming, Konten und Cloudspeicherung.

## 3. Nachgewiesener Ausgangsstand

Legende: **Basis fertig** = implementiert und automatisiert geprüft;
**teilweise** = untere Schichten vorhanden, Produktweg unvollständig;
**offen** = noch zu bauen oder manuell abzunehmen.

| Bereich | Stand | Beleg / Grenze |
|---|---|---|
| Rust/egui-Fenster, sechs Ziele, Themes, DE/EN | Basis fertig | `src/ui`, Nutzer-Screenshots; Design noch nicht freigegeben |
| Reducer, Snapshots, Generationen, begrenzte Queues | Basis fertig | `src/app`, `app_handle.rs`, `backend_bridge.rs` |
| Start/Stop bei voller Queue | Basis fertig | `8e04492`, echte Ablehnung und Rückstau getestet |
| Discovery, Capture, Mehrraumtransport, Recovery | Code-/Testbasis vorhanden | `src/backend`, `cast.rs`, `airplay-*`; Langzeit-/Skew-Messung und Release-Gates offen |
| Drei gleichzeitig spielende Empfänger | owner-bestätigt | Alle drei spielen gleichzeitig (2026-09-09); Dauer und gemessener inter-Speaker-Skew noch offen |
| Mute, Latenz, Quellenanzeige/-auswahl | Softwarebasis fertig; P2.5 offen | `951aa87`, `e9b7dde`: produktiver begrenzter Scan, Refresh und Fehlerstatus; reale Endpunktwechsel noch abzunehmen |
| Gespeicherte Gruppen | Softwarebasis fertig | `21af72b`, `4373e6b`, `38d3bc8`, `9eebc0e`: Projektion, lokale Entwürfe, CRUD, Apply/Start, exakte Resultate; native/physische Gates bleiben |
| Einzelpegel und Receiver-Retry | Einzelpegel implementiert; Retry-UI offen | `1ac5c31`, `4ce6b7d`–`a5136e6`: kompakte Regler, sichere Startreihenfolge und neuester Pegel; Hardwarebestätigung historisch |
| Auto-Connect | teilweise | Backend implementiert; vollständige Einstellung/Projektionssemantik fehlt |
| Persistentes Pairing | Bibliotheksbasis, Sicherheitsarbeit offen | Feste Fallback-PIN und Klartext-Identitätsdatei im Bibliothekspfad, kein fertiger App-PIN-Workflow |
| Diagnose und Kalibrierung | teilweise | Registry/Export/Kalibrierung vorhanden; GUI zeigt nur kleinen Ausschnitt |
| Windows-Autostart | offen | OS-Integration und bestätigte UI fehlen |
| Releasepaket | teilweise | `build.ps1`, historisches `dist`-Artefakt; keine CI-Workflows, aktuelles Releasepaket fehlt |
| Upgrade-/Downgrade-Datenschutz | teilweise | Migrationen/atomare Speicherung vorhanden; unbekannte Shell-Version kann beim nächsten Speichern überschrieben werden |
| Native Sicht-/Accessibility-Abnahme | offen; aktuelle Automation blockiert | Debug-Build `b639d30` startete; spätere Fixes bis `047dfc6` nicht nativ gestartet. Screenshotfehler `SetIsBorderRequired`/`0x80004002`, UIA zeigt nur Fensterchrome; keine visuelle/navigationelle Abnahme oder vollständige Matrix |

Historische vollständige Verifikation vor dieser Planungsrunde:
**1.976 Tests bestanden, 6 ignoriert**, normaler Debug-Build erfolgreich,
Clippy ohne Fehler mit Bestandswarnungen. Das ist keine neue Testausführung
dieser Planungsrunde und keine Hardware-/Releasefreigabe.
Die aktuelle Prüfung der Fünf-Punkte-Umsetzung steht in der verlinkten
Releaseprüfung; automatisierte Softwarebelege und ungetestete physische Gates
werden dort getrennt geführt. Die offenen Gesamtplan-Gates bleiben verbindlich.
Die beiden historischen Task-7-Korrekturrunden waren auf `3595057` freigegeben;
dessen **2.092 bestandene Tests, 0 Fehler, 15 ignorierte Fälle** sind historische,
nicht aktuelle Belege. Gesamtbranch-Review und einzige finale Korrekturrunde
sind durchgeführt. `047dfc6` schließt alle drei ursprünglichen Befunde:
tatsächliche Audiofreigabe mit neuestem Pegel, bestätigte Master-Wiederherstellung
und erneute Auswahl von Offline-Mitgliedern.
**Software NICHT merge-bereit; Important-Restbefund OPEN:** Suspend während
vorbereiteter Aktivierung verliert nach Cleanup die Stopped-Meldung dieser
Generation; Cancel vor dem Ende der zwei Sekunden langen Resume-Beruhigung
kann die Shell in Stopping festhalten. Nächste autorisierte Quellcodearbeit
muss „Suspend während Aktivierung → Cancel vor Resume-Settle“ rot reproduzieren,
Stopped nach begrenztem Cleanup erhalten und fokussiert reviewt sowie frisch
verifiziert werden. Die finale Korrekturwellengrenze ist erreicht: keine zweite
Quellcode-/Teständerung im Verifikationsabschluss. Grüne Bestandstests ersetzen
den fehlenden Regressionstest nicht. Kein neuer Hardware-/Native-Abnahmebeleg;
aktuelle Softwarebelege, EXE-Pfad und Hash stehen getrennt in der
[Releaseprüfung](../../RELEASE_CHECK_2026-09-09.md).

Frische vollständige Verifikation von
`047dfc6070cf42d8404aca779dd17c57e5af6358`: **2.104 bestanden, 0 fehlgeschlagen,
15 ignoriert**, 44 Workspace-Ziele inklusive Doctests. Scoped Clippy und normaler
Debug-Build jeweils Exit 0 mit bestehenden Warnungen. Die normale Debug-EXE
(nicht gestartet) hat SHA-256
`55201E6BEB941BBE07F0957C5A23744DFE2D8DC7C0BD7142B069E0B5B71B12A6`.
Diese Ergebnisse schließen weder den Important-Suspend/Cancel-Restbefund noch
die native oder physische Abnahme: Software weiterhin NICHT merge-bereit.

## 4. Ziel-UI: Windows Native, aber vollständig bedienbar

### Gemeinsame Regeln

- Die vorhandene Farbwelt bleibt: Light `#F4F6FB`/`#FFFFFF`, Dark
  `#10151F`/`#18202D`, Akzent `#3168E8`/`#7EA6FF`, Status über semantische Tokens.
  Aktueller Code und Kontrastprüfungen gehen historischen Farbtabellen vor.
- Segoe UI Variable mit Inter-Fallback; 28/34 Seitentitel, 20/26 Statusüberschrift,
  16/22 Abschnitte, 14/20 Fließtext, 12/17 Nebentext. Monospace nur für Messwerte.
- Native Fensterleiste und sechs stabile Seiten bleiben. Navigation 176 bzw.
  64 logische Punkte, Umschaltung bei 1000; Seitenrand 24 bzw. 16.
- Informationen primär durch Ausrichtung, Abstände und klare Zeilen gliedern.
  Nicht jede einzelne Einstellung braucht eine große eigene Karte.
- Controls mindestens 40 Punkte, Navigation mindestens 44. Sichtbare
  Radiomarkierungen dürfen klein sein, aber die gesamte beschriftete Zeile
  muss bedienbar sein. Kein enges Aneinanderreihen kleiner Klickziele.
- Höchstens eine dominante gefüllte Aktion pro Kontext. Stop/Abbrechen wird
  nie von Gruppenbearbeitung, Speichern oder Fehleraktionen verdrängt.
- Keine Entwicklungsankündigungen, Fantasiemetriken oder Attrappen-Buttons.
  Ein Leerzustand hat einen realen nächsten Schritt, wenn einer existiert.
- Statisch dargestellte Screenshots sind kein Beleg für Hover, Fokus, Scrollen
  oder andere Themes; diese Zustände erhalten separate Abnahmefälle.

### Übersicht: tägliche Steuerung

Eine kompakte Status-/Routenzeile ersetzt den großen weitgehend leeren Block.
Quelle und Ziele sind lesbar, auch ohne grafische Verbindungslinie. Bei leerer
Auswahl steht „Lautsprecher auswählen“ statt „Pfad inaktiv“ im Vordergrund.

```text
Übersicht                                      [Streaming starten/stoppen]
Windows-Audio · tatsächliche Quelle → 2 ausgewählt · 2 übertragen
Status und gegebenenfalls eine konkrete Handlungsempfehlung

Lautsprecher                                    [Aktualisieren]
[✓ Wohnzimmer · Verfügbar]       [✓ Büro · Verfügbar]
[  Badezimmer · Verfügbar]

Gesamtlautstärke  [────────●─────]  65 %             [Stummschalten]
                       [Änderungen anwenden] [Verwerfen]  (nur bei Entwurf)
```

Die Routenzeile benötigt im normalen einzeiligen Zustand ungefähr 88–112
Punkte, darf für lange Texte wachsen. Der Header bleibt erreichbar; nur
Inhalt scrollt. Der Slider darf optisch einen kleineren Griff erhalten,
ohne das mindestens 40 Punkte hohe Bedienfeld zu verkleinern.
Standardanzeige nennt Auswahl und echte Wiedergabe getrennt. Keine dritte
wiederholte Statuszeile je Empfänger, solange kein Fehler vorliegt.

### Lautsprecher: verwalten und auswählen

Eine kompakte Liste statt breiter passiver Karten. Pro Zeile: Auswahl, Name,
Modell, Erreichbarkeit, tatsächliche Wiedergabe und bestätigter Einzelpegel.
Details öffnen sich innerhalb derselben Seite; Fehler und Retry am betroffenen
Gerät. Die ganze Zeile darf nicht gleichzeitig Auswahl und Slider aktivieren.
Übersicht und Lautsprecher verwenden denselben Auswahlentwurf und dieselbe
Änderungsleiste. Filter „Alle / Ausgewählt / Aufmerksamkeit“ und Namenssuche
werden erst benötigt, wenn die vollständige Liste nicht bequem überblickbar ist;
sie ändern nie Auswahl oder Identität.

### Gruppen: echte gespeicherte Kombinationen

Liste mit Name, Mitgliedern, Verfügbarkeit und primärer Aktion „Übernehmen“.
„Neue Gruppe“ öffnet einen Editor mit Namen, Mitgliedern und optional deren
Pegelwerten. Speichern startet keinen Stream. Löschen bestätigt den exakten
Gruppennamen und beendet keine aktive Wiedergabe. Eine fehlende/offline
Komponente bleibt sichtbar und wird nicht aus der gespeicherten Gruppe entfernt.
Leere Seite: kurzer Zwecktext und funktionierendes „Gruppe erstellen“.

### Audio: Quelle und Wiedergabe unterscheiden

Kompakte Einstellungszeilen: „Audioquelle“, „Stummschaltung“, „Latenz“.
Quelle ist das Windows-Wiedergabegerät, dessen Audio per Loopback aufgenommen
wird; die Bezeichnung „Aufnahmegerät“ darf nicht ein Mikrofon suggerieren.
„Windows-Standard“ und der tatsächlich verwendete Endpunkt werden unterschieden.
„Bereit“, „Audio wird erfasst“ und „Wird an Lautsprecher übertragen“ sind
verschiedene Zustände. Stop beendet die produktive Audioerfassung, sofern keine
explizite, sichtbare Testfunktion sie benötigt; Endpunkterkennung darf weiterlaufen.
Nicht verfügbare Latenzen tragen einen kurzen konkreten Grund.

### Diagnose: hilfreich, auch wenn gerade kein Stream läuft

Ohne Sitzung: Discovery-/Quellen-/Appzustand, Versionsinformation und sichere
Zusammenfassung; keine leere Vollseite. Während einer Sitzung: Gesundheit,
Empfänger, Timing-Referenz und Messwertalter. Erweiterte Bereiche für Ereignisse,
Kalibrierung und technische Details, keine zweite Hauptnavigation.
„Zusammenfassung kopieren“, „Supportbericht speichern“ und „Logordner öffnen“
sind echte Aktionen mit Ergebnis-/Fehlerrückmeldung. Copy/Export nutzt eine
Allowlist, nicht den sichtbaren Bildschirmtext oder rohe Logs.

### Einstellungen: kurze, ausgerichtete Zeilen

Bereiche Darstellung, Sprache, Verhalten, Verbindung, Tastatur, Informationen.
Theme/Sprache mit ausreichend großen Auswahlfeldern; Verhalten und Auto-Connect
als getrennte Schalter. „Mit Windows starten“ bedeutet nicht „Audio starten“.
Hotkey wird als Tastenkombination erfasst und vor Übernahme gegen Konflikte und
reservierte Kombinationen geprüft; bestehendes Kürzel bleibt bei Fehler gültig.
Explizites „Beenden“ auch im Fenster zugänglich, damit der Tray nicht der einzige
Ausgang ist. Statusfuß zeigt in breiter Navigation auf jeder Seite Text und Symbol.

## 5. Architektur und gemeinsame Verträge

```text
UI / Tray / Hotkey
        ↓ AppEvent
App-Reducer: lokaler Entwurf, ausstehende Aktionen, Präsentationszustand
        ↓ begrenzter Effekt
backend_bridge.rs → DeviceBackendHandle → Backend / Persistenz / Session
        ↑ bestätigter Snapshot + korreliertes Ergebnis
UiSnapshot / sichere Diagnoseansicht → reine Renderer
```

1. Die vier vorhandenen `AppHandle`-Methoden bleiben stabil. Keine Sessions,
   Sockets, Adressen oder geheimen Schlüssel in UI-Modelle einschleusen.
2. **Auswahl-Vertrag vor Gruppen/Auto-Connect klären:** Der heutige Shell-Code
   behandelt gewünschte Mitgliedschaft als Shell-eigen; gespeicherte Gruppen
   und Auto-Connect verändern sie im Backend. Ziel: Backend besitzt die
   dauerhaft angewandte Auswahl; Shell besitzt nur den noch nicht angewandten
   Entwurf. Bestätigung mit Revision übernimmt angewandte Werte. Ein lokaler
   Entwurf bleibt bei konkurrierender Änderung bestehen und wird als veraltet
   markiert. Keine zwei unabhängig schreibenden Wahrheiten.
3. Neue Aktionen führen Request-ID, betroffenen Gegenstand und Ergebnis mit.
   Busy, ungültige Anfrage, nicht unterstützte Fähigkeit, Speicherfehler und
   geschlossener Dienst sind verschiedene lokalisierte Antworten.
   Verlorene Broadcast-Events dürfen kein dauerhaftes „Speichert“ erzeugen;
   Ergebnisse müssen über Snapshot/retained Feedback rekonstruierbar sein.
4. Gerätebezogene Einstellungen zeigen bestätigte Werte; lokale Eingabeentwürfe
   werden explizit getrennt. Nicht aus fehlenden Daten „0“ oder „Erfolgreich“ ableiten.
5. Bestehende Domänen wiederverwenden: `backend/command.rs`, `backend/model.rs`,
   `backend/controller.rs`, `backend/persistence.rs`, `calibration.rs`,
   `diagnostics.rs`. Kein zweites `app/device_bridge.rs` neben der bestehenden
   Bridge erzeugen, nur weil ein alter Plan diesen Dateinamen nennt.
6. Persistente Geräte-/Gruppendaten bleiben im Backend, reine UI-Präferenzen
   in `preferences.rs`. Neue Schemas migrieren alte Dateien verlustfrei;
   neuere unbekannte Versionen nicht blind überschreiben.
7. UI bleibt ohne I/O und ohne dauernde Repaint-Schleife. Diagnose höchstens
   4 Hz sichtbar, keine visuelle Abfrage verborgen/minimiert. Queues begrenzt.
8. Protokolländerungen erst nach Lesen von `AIRPLAY_2_SPEC.md` und der bestehenden
   Implementierung. Keine erfundenen Timingwerte oder Audio-Pipeline-Duplikation.

## 6. Teilprojekte und vollständiger Task-Index

Neue IDs `P1.1` usw. sind absichtlich unabhängig von historischen „Task 1“.
Alle folgenden Checkboxen bleiben offen, bis ihr jeweiliges Produktziel geprüft
ist. Vorhandene Teilimplementierungen verkürzen die Arbeit, ersetzen aber kein Gate.

### P0 — technische Basis: erledigt, Releaseprüfung bleibt offen

- [x] P0.1 Zentraler Reducer, UI-Snapshots, native Shell und aktiver Backendpfad.
- [x] P0.2 Ein-Capture-Mehrraumtransport und automatisierte Recovery-Grundlagen
  implementiert; aktuelles Hardware-Gate fehlgeschlagen, siehe P6.
- [x] P0.3 Audiozustands-, Fokus-, Geräteidentitäts- und Busy-Queue-Fixes.
- [x] P0.4 Aktueller integrierter Stand gebaut und getestet: `8e04492`.

### P1 — täglicher Bedienablauf und visuelle Struktur

- [ ] **P1.1 Kompakte Übersicht:** Route/Status/Quelle zusammenführen,
  verständlicher Nullzustand, ein eindeutiger Start-/Stop-Bereich.
  Dateien: `ui/pages/overview.rs`, `components/route_ribbon.rs`, `presentation.rs`.
  Gate: alle Sessionphasen und 0/1/2/8 Empfänger, kein falsches Live-Segment.
- [ ] **P1.2 Auswahl auf beiden Seiten:** Übersicht und Lautsprecher steuern
  denselben Entwurf; Anwenden/Verwerfen bleibt konsistent über Seitenwechsel.
  Dateien: `pages/speakers.rs`, `receiver_card.rs`, `app/reducer.rs`.
  Gate: Reorder, Offline-Gerät, geänderte Discovery und volle Queue erhalten
  Auswahl; Klick auf einen Einzelregler selektiert nicht versehentlich die Zeile.
- [ ] **P1.3 Kompakte Controls und Settings:** sinnvolle Gruppen statt vieler
  großer Karten, ausreichende Radio-/Checkbox-Ziele, dauerhafter Statustext,
  verständliche Quelle-/Capture-Texte. Dateien: `pages/audio.rs`, `settings.rs`,
  `app_shell.rs`, `audio_dock.rs`, `i18n.rs`, `theme.rs`.
  Gate: vollständige Texte und erreichbare Controls in beiden Mindestgrößen.
- [ ] **P1.4 Einheitliche Zustands-/Leer-/Fehleransichten:** keine
  Zukunftsankündigungen, reale Navigation/Retry, Standardmodus ohne unnötige
  Techniktexte. Dateien: `empty_state.rs`, `notice.rs`, `presentation.rs`.
  Gate: tatsächliche Aktionen, nicht nur geänderte Texte; DE/EN vollständig.
- [ ] **P1.5 Visueller Checkpoint:** Screenshotmatrix und Tastaturprüfung
  der überarbeiteten Tagessteuerung; Designanleitung mit abgenommenen Beispielen
  aktualisieren. Kein „fertig“ ausschließlich aus Headless-Tests.

### P2 — vollständige Geräte-/Audioverträge

- [ ] **P2.1 Angewandte Auswahl und Ergebnisse:** den Vertrag aus Abschnitt 5
  in Bridge, `app/{state,event,effect,reducer,snapshot}.rs` integrieren.
  Gate: Neustart stellt dieselbe Auswahl dar; konkurrierende Backendrevision
  erhält lokalen Entwurf; abgelehnte und veraltete Aktionen bleiben korrekt.
  **Software umgesetzt 2026-09-09:** `21af72b`, `4373e6b`, `9eebc0e`;
  bestätigte Auswahl, Revisionen und exakte Operationsresultate sind geprüft.
  Die native Neustartabnahme bleibt ausstehend.
- [ ] **P2.2 Audioendpunkt-Scans fertigstellen:** Bereits vorhandene reale Enumeration in
  `RealWasapiApi::enumerate_render_endpoints` und `WasapiCaptureSource::endpoints`
  produktiv aufrufen: off-thread Scans mit Begrenzung, Launch-/Quellenwechsel-/
  Wiederkehr-/Resume-Triggern und Unterscheidung unbekannt vs. gemessen leer.
  **Software umgesetzt 2026-09-09:** `951aa87`, `e9b7dde` rufen den vorhandenen
  Endpunktpfad produktiv außerhalb des Actors auf, begrenzen hängende Scans und
  zeigen Refresh-/Fehlerstatus. WIP `469b530` ist keine offene Portierungsaufgabe.
  Dateien: `backend/capture.rs`, `backend_bridge.rs`, `pages/audio.rs`.
  Gate: Defaultwechsel, Auswahl, Entfernen, Wiederkehr, leere Liste, doppelter
  Anzeigename; Identität nie aus Position/Name ableiten. Hängender/fehlgeschlagener
  COM-Scan blockiert den Actor nicht und löscht nicht das letzte gültige Ergebnis.
- [ ] **P2.3 Aufnahmelebenszyklus:** Capture nur für aktive/aufbauende Session
  oder expliziten Test; Discovery/Endpunkterkennung unabhängig weiterbetreiben.
  Dateien: `backend/capture.rs`, `backend/controller.rs`, `cast.rs`.
  Gate: Stop/Abbruch beendet Audioerfassung begrenzt; erneuter Start öffnet
  genau eine Quelle; keine unbounded Puffer oder verwaisten Threads.
- [ ] **P2.4 Einzelpegel, Master und Retry:** `SetReceiverLevel`,
  `SetMasterVolume`, `SetMuted`, `RetryReceiver` Ende zu Ende anschließen.
  Gate: effektiver Pegel folgt vorhandenem Backendmodell; Master-Mute erhält
  konfigurierte Pegel; Änderung betrifft nur den adressierten Link und braucht
  keinen Neustart. Ein wiederkehrender Empfänger erhält seinen Pegel vor dem
  ersten Audio; ein Fehler am Gerät blockiert keine anderen Controls.
  **Teilfortschritt 2026-09-09:** Einzelregler auf Übersicht/Lautsprecher,
  bestätigte Pegel-Snapshots, Speicherung und Live-Weiterleitung implementiert.
  `4ce6b7d`, `c4b8ebe`, `08d83c8` und `a5136e6` prüfen die Pegelanwendung vor
  produktivem Audio-Start und den Vorrang späterer Pegeländerungen. Die
  Eigentümerbestätigung vom 2026-09-09 belegt die historische Hörprobe der
  Einzelregler. Frische physische Start-/Rejoin-Abnahme und Retry-UI bleiben
  für das volle P2.4-Gate offen.
- [ ] **P2.5 Latenz und Befehlsfehler:** angebotene Presets und Gründe aus dem
  Backend; Busy-Hinweis und Speicherfehler ohne falsches SessionFailed.
  Gate: Ablehnung erhält bestätigten Wert; restartpflichtige Änderung wird vor
  ihrer Anwendung erklärt; doppelte Antworten erzeugen keine Hinweisschleife.

### P3 — gespeicherte Gruppen

**Software umgesetzt und automatisiert geprüft, 2026-09-09:** `21af72b`,
`4373e6b`, `38d3bc8`, `9eebc0e` sowie die Start-/Stop-Integration aus der
Releaseprüfung. Der Editor lebt seitenlokal in `pages/groups.rs`.
Die folgenden Produktgates sind damit softwareseitig abgedeckt, bleiben aber
für native Neustart-, Wiedergabe- und Mehrraumabnahme als Gesamtgates offen.

- [ ] **P3.1 Sichere Gruppenprojektion:** IDs, Name, Mitglieder, Pegel,
  Verfügbarkeit und ausstehende Aktion im Shellmodell; keine Backendobjekte im View.
  Dateien: `backend_bridge.rs`, `app/{state,event,effect,snapshot}.rs`.
  Gate: Snapshot nach Neustart und nach Backendänderung gleichwertig dargestellt.
- [ ] **P3.2 Erstellen/Bearbeiten:** vorhandenes `SaveGroup { id, name, members }`
  verwenden; Editorzustand getrennt von gespeicherter Gruppe und Auswahlentwurf.
  Datei: `pages/groups.rs`, neuer seitenlokaler `ui/pages/group_editor.rs`.
  Gate: leerer/zu langer/doppelter Name, offline Mitglieder, Speicherablehnung,
  Abbruch und Doppelklick verursachen keine doppelten Gruppen. Reine CRUD-
  Änderungen an Vorlagen starten oder unterbrechen keine aktive Sitzung.
- [ ] **P3.3 Übernehmen/Starten:** `ActivateSavedGroup { id, start }` verknüpfen;
  bereits offener Auswahlentwurf verlangt bewusstes Verwerfen/Beibehalten.
  Gate: Übernehmen startet nicht; Übernehmen-und-Starten nutzt exakt die Gruppe;
  Änderung während Streaming kündigt kontrollierten Neustart an.
- [ ] **P3.4 Löschen/Persistenz:** `DeleteGroup` mit Bestätigung, danach
  bestätigter Listenstand. Gate: aktive Auswahl und laufende Session bleiben
  beim Löschen der Vorlage erhalten; gelöschte Gruppe kehrt nicht nach Neustart zurück.

### P4 — Windows-Alltag und Einstellungen

- [ ] **P4.1 Auto-Connect:** `SetAutoConnect` und bestätigter Wert in Settings;
  letzte angewandte Auswahl aus Backendpersistenz. Gate: standardmäßig aus,
  nur einmal nach initial stabiler Discovery, expliziter Stop wird nicht aufgehoben.
- [ ] **P4.2 Autostart und Prozessverhalten:** neue Plattformkomponente
  `platform/startup.rs` für benutzerbezogenen, deaktivierbaren Autostart.
  Portable Pfadänderung/fehlende EXE erkennen, keine Adminpflicht. Einzelinstanz
  und Show-existing-window prüfen bzw. ergänzen. Gate: An-/Abmelden,
  wiederholter Start, Pfad mit Leerzeichen, deaktivierter Eintrag, Upgrade.
- [ ] **P4.3 Tray, Beenden und Hotkey fertigstellen:** bestehende Plattform-
  und Traymodule nutzen, explizites Beenden im Fenster, konfliktfeste Kürzeleingabe.
  Gate: Explorer-Neustart, Verbergen/Wiederherstellen, Hotkeywechsel, gesperrtes
  Kürzel und Quit während Verbindungsaufbau; kein zweiter Backendprozess.
- [ ] **P4.4 Sichere Updates und Datenhaltung:** `preferences.rs` und
  `backend/persistence.rs` auf eine explizite Upgrade-/Downgrade-Policy bringen.
  Shell-Settings unbekannter neuerer Version vor Überschreiben schützen;
  ungültige Originaldaten recoverbar erhalten, nicht nur mit Defaults verdecken.
  Gate: Shell v1/v2, Device v1/v2, Legacy-Hotkey/-Pegel, unbekannte Version,
  unterbrochene atomare Speicherung und nicht-ASCII-Profilpfade. Portable
  Update ersetzt bei beendeter App nur die EXE; Datenlöschung ist getrennt,
  optional und niemals eine Nebenwirkung von Update/Start.

### P5 — Diagnose, Support und manuelle Kalibrierung

- [ ] **P5.1 Sichere Diagnoseansicht:** `diagnostics.rs`/Registry ohne zweite
  Sammlung verwenden; sichere Receiver-/Ereignismodelle und Freshness ergänzen.
  Dateien: `backend_bridge.rs`, `app_handle.rs` nur falls accessor nötig,
  `pages/diagnostics.rs`. Gate: keine Sitzung, fehlende/veraltete Werte,
  begrenzte Ereignisliste; höchstens 4 Hz sichtbar, keine UI-Abfrage versteckt.
- [ ] **P5.2 Kopieren/Export/Logordner:** vorhandenen Export off-thread anbinden,
  Dateidialog und Clipboard über Plattform-Effekte; Fortschritt und Ergebnis.
  Gate: Abbrechen, gesperrtes Ziel, volle Queue, große Ereignismenge, Redaction;
  vorhandene Größenbegrenzung von 4 MiB beibehalten; keine automatischen Uploads
  und keine Rohlogs als „sicherer Bericht“. Logordner-Aktion nur anbieten, wenn
  es einen echten dokumentierten Dateilog-Pfad gibt; sonst zuerst begrenztes,
  rotierendes, redigiertes Logging implementieren.
- [ ] **P5.3 Manuelle Kalibrierung:** vorhandene `CalibrationCommand` und
  effektive Werte nutzen, Entwurf/Anwenden/Zurücksetzen/Testton klar trennen.
  Zuerst `ControllerActor.calibration` als sicheren Profil-/Status-/Effektivwert-
  Snapshot samt Auswahlrevision veröffentlichen; das fehlt heute im DeviceSnapshot.
  Gate: veraltete Auswahlrevision wird abgewiesen; exakt ein kontrollierter
  Neustart; keine Änderung gemeinsamer RTP-Zeitstempel zur Vortäuschung von Sync.
- [ ] **P5.4 Konkrete Fehlerhilfe:** Discovery, Quelle, Zugriff/Pairing,
  Callback/Firewall, Retry und Persistenz als typisierte Ursachen darstellen.
  Gate: keine rohe Fehlermeldung/Adresse/Schlüssel im Screenreader oder Export;
  Hinweis „Firewall prüfen“ bleibt Verdacht, solange Ursache nicht bewiesen ist.

### P6 — Transport- und Hardwarequalität

- [ ] **P6.1 Mehrraum-Recovery-Gates:** vorhandene Isolation/Rejoin-Logik
  mit Sekundärverlust, Primärverlust, Netzwechsel, Sleep/Wake und Quellenwechsel
  prüfen. Dateien: `backend/{session,controller,discovery,capture}.rs`,
  `tests/{device_resilience,session_recovery,system_recovery}.rs`.
  Gate: Sekundärverlust beendet verbleibende Wiedergabe nicht; Primärverlust
  erzeugt begrenzten, klar sichtbaren Wiederaufbau statt stillen Totalausfalls.
  Aktueller Reviewbefund 2026-09-08: Abbruch von `connect_group_best_effort`
  verwirft noch lokal gehaltene Links ohne garantiertes TEARDOWN; das spätere
  `client.disconnect()` kennt diese Links nicht. Außerdem verschluckt
  `AirPlayClient::disconnect` Fehler beim Beenden sekundärer Links. Beide
  Lebenszyklus-/Fehlerrückgabeverträge brauchen eigene Regressionstests und
  Korrekturen. Kein „sauber beendet“ aus dem aktuellen Gesamt-Ok ableiten.
- [ ] **P6.2 Synchronität und Ressourcen:** 30-Minuten-Hardwarelauf, längerer
  Soak, 100 Start/Stop-Zyklen sowie 8 simulierte Empfänger. Gemeinsame Zeitbasis,
  begrenzte Speicher-/Queue-Nutzung und keine wachsenden Tasks/Sockets nachweisen.
  Gate: akustische Bewertung getrennt von Scheduler-/PTP-Messung dokumentieren;
  nur wirklich gemessene Toleranzen veröffentlichen.
- [ ] **P6.3 Pairing absichern und Kompatibilität entscheiden:** Der derzeitige
  Bibliothekspfad in `airplay-client/src/{connection,group}.rs` verwendet eine
  feste Fallback-PIN und serialisiert den Ed25519-Secret in eine CWD-relative
  JSON-Datei. **Unabhängig von einer Apple-TV-UI-Freigabe ist dieser Punkt ein
  Release-Sicherheitsgate:** Credential-Store-Grenze mit Windows-geschützter
  Speicherung/Migration einführen oder betroffenen persistierenden App-Pfad
  explizit sperren; keine stillen Klartext-Neuschreibungen oder festen PIN-Versuche.
  Keine bestehenden Identitätsdateien ohne bestätigten Migrations-/Recoveryweg löschen.
  Anschließend HomePodvarianten und Apple-TV-PIN-Fähigkeit prüfen. Bei Freigabe:
  abbrechbarer Dialog, begrenzte Versuche, kontrolliertes Re-Pairing, keine
  Geheimnisse in Logs/Events/Snapshots. Sonst Capability als nicht unterstützt ausweisen.
- [ ] **P6.4 Timing-Präzision:** `998f817` read-only reviewen; mathematische
  Offsets, Peer-Delay-Antworten und Fallback unterscheiden. Eigener begrenzter
  Implementierungsplan nur bei bestätigtem Bedarf/Mehrwert, dann echte Geräteprüfung.
  WIP-Pakettypen/Tracker allein reichen nicht: dort ist der Tracker noch nicht
  in den produktiven Slave-Loop eingebunden. Kein Erfolg nur anhand seiner Unit-Tests.

### P7 — vollständige Produktabnahme

- [ ] **P7.1 Automatisierte Integrationsmatrix:** jeder sichtbare Befehl vom
  Widget bis zum bestätigten Snapshot oder lokalisierten Fehler. UI-Matrix,
  Migrationen, Backpressure und Nichtblockieren einschließen.
- [ ] **P7.2 Native Accessibility-/Designmatrix:** alle sechs Seiten, Dark,
  Light, High Contrast, DE/EN, 100/125/150/175/200 %, 900×600/1120×720,
  Mixed-DPI, Narrator/NVDA, Tastatur, Remote Desktop. Nutzer-Screenshots ergänzen,
  aber ersetzen die interaktive Prüfung nicht.
- [ ] **P7.3 Performance und Dauerbetrieb:** sichtbarer und verborgener Idle
  unter 1 % CPU über 60 s auf dokumentiertem Referenzrechner; 30-Minuten-Stream
  und Soak getrennt; keine unbeschränkte Speicher-/Handle-Zunahme.
- [ ] **P7.4 Clean-Machine-Abnahme:** normale Benutzerrechte, fehlende Fonts,
  Offline-Start, beschädigte/alte Einstellungen, Ausgabepfad mit Leerzeichen,
  Firewallrestriktion, keine Konsole/Runtime-Nachinstallation im Normalbetrieb.

### P8 — Veröffentlichung und wartbarer Abschluss

- [ ] **P8.1 Releasepipeline:** `build.ps1` und CI auf reale, nicht leere
  Testfilter prüfen, `--locked`, tatsächliche MSRV prüfen, Release-EXE bauen,
  versioniertes Paket und SHA-256 erzeugen. Neue Workflows
  `.github/workflows/windows.yml` und `.github/workflows/release.yml` sind zu
  erstellen; heute gibt es keine. Gesamten ausgelieferten Abhängigkeitsbaum
  einschließlich transitiver Lizenzen/Notice-Texte prüfen, nicht nur die
  ausgewählten Bibliotheken in `THIRD_PARTY_NOTICES.md`. Portable ZIP enthält
  EXE, Lizenz-/Notice-Material und Kurzinfo; keine PDBs, Logs, Identitäten,
  Supportberichte oder Assistant-Dateien. PE-Version, Icon und GUI-Subsystem
  mit Tag vergleichen. Historisches `dist/OpenAirCast.exe` nicht wiederverwenden.
- [ ] **P8.2 Dokumentation:** README/VALIDATION/NEXT_STEPS auf denselben Commit
  und dieselbe Supportmatrix bringen; Installation, Update, Autostart,
  Datenorte, Fehlerhilfe und bekannte Grenzen verständlich beschreiben.
  Kurze Datenschutz-/Datenlöschanleitung ergänzen; erwarteten lokalen Netzwerkverkehr
  von Internetzugriff unterscheiden und durch einen tatsächlichen Netzwerktest
  absichern. Firewallhilfe erklärt gegebenenfalls einen einmaligen gesondert
  privilegierten Schritt, verlangt aber keinen dauernden Adminbetrieb der App.
  Testprotokolle enthalten Version/Umgebung, keine privaten Gerätekennungen.
- [ ] **P8.3 Releaseentscheidung:** alle Pflicht-Gates und offenen Fehler prüfen;
  Quellcode, GPL-/Upstream-/Drittanbieterhinweise und Artefakte zusammenhalten.
  Release-Tag/Veröffentlichung erst nach expliziter Freigabe; nicht automatisch
  `master` überschreiben oder eine getestete EXE mit ungeprüftem Code ersetzen.

## 7. Reihenfolge und parallele Arbeit

```text
P1 Tagessteuerung ────────────────┐
P2 Geräte-/Bestätigungsverträge ─┼→ P3 Gruppen ─→ P4 Alltag
                               └→ P5 Diagnose/Kalibrierung
P6 Transportprüfung ────────────────────────────┤
                                              ↓
                                     P7 Produktabnahme
                                              ↓
                                     P8 Veröffentlichung
```

- Aktuelle Ausnahme aufgrund Nutzerrückmeldung: Mehrraum-Regression vorziehen;
  Verbindungsaufbau, Zeitbasis und Audio getrennt nachweisen. Anschließend
  P1.1–P1.5 als sichtbarer Checkpoint, keine neue Gruppenfunktion nebenbei.
- P2.1 ist gemeinsames Fundament; vor P3 und P4.1 fertigstellen.
- Nach P2 können Gruppen, OS-Integration und Diagnose in getrennten Dateien
  parallel entwickelt werden; Änderungen an `app/*` und `backend_bridge.rs`
  werden von genau einem Integrator serialisiert.
- P6-Leseprüfung/Testdesign kann neben UI-Arbeit laufen. Keine konkurrierenden
  Hardware-Streams auf denselben Empfängern und keine gleichzeitigen Eingaben
  mehrerer Agents in dieselbe native App.
- Jeder Task: RED → kleine Änderung → GREEN → unabhängige Review → Commit,
  jeweils nur bei vorhandener Commitfreigabe. Jeder Teilprojektabschluss ist
  ein eigener Pausen-/Reviewpunkt; kein unkontrollierter Gesamt-Rewrite.
- Große bestehende Dateien nur entlang tatsächlich benötigter Verantwortungen
  aufteilen, nicht als unabhängiges Refactoringprojekt.

## 8. Release-Gates und Nachweisformat

| Gate | Erforderlicher Nachweis | Blockiert 1.0? |
|---|---|---|
| Alltag | Auswahl → Anwenden → Start → Pegel/Mute → Stop, Fenster/Tray/Hotkey konsistent | Ja |
| Fähigkeiten | Gruppen, Einzelpegel, Quelle, Auto-Connect, Autostart, Diagnose/Kalibrierung Ende zu Ende | Ja |
| Zuverlässigkeit | Hardware-Ausfälle, Wiederkehr, Sleep/Wake, 30 Minuten und Wiederholungen | Ja |
| Bedienbarkeit | Native Theme-/DPI-/Tastatur-/Screenreadermatrix mit Befunden geschlossen | Ja |
| Daten/Support | Migration, fehlgeschlagene Speicherung, Export-Allowlist, kein unsicherer persistenter Pairingpfad | Ja |
| Auslieferung | Clean Windows, normale Rechte, reproduzierbar beschriebener Releasebuild, Lizenzen | Ja |
| Peer Delay | Präzisionsnachweis oder dokumentierte getestete Begrenzung des vorhandenen Pfads | Wenn Sync-Gates sonst scheitern |
| Signierung/Installer/Updater | Eigene Infrastruktur und separate Freigabe | Nein für portable 1.0 |

Jeder Nachweis nennt Commit, EXE-Hash, Datum, Windows-/Gerätesoftwareversion,
Testschritte, erwartetes und beobachtetes Ergebnis, Dauer und verbleibende
Abweichung. „Nicht getestet“, „nicht messbar“ und „fehlgeschlagen“ sind getrennte
Zustände, nie ein grünes Häkchen. Ein abgebrochener oder ignorierter Test zählt
nicht als bestanden. Keine automatische Veröffentlichung von Gerätekennungen.

## 9. Vor Freigabe ausdrücklich zu bestätigen

Dieser Entwurf empfiehlt folgende Produktgrenzen zusammenhängend:

1. Bestehende Rust/egui-App behalten, alle sechs Seiten funktional fertigstellen.
2. Portable HomePod-orientierte Version 1 zuerst; Installer/Updater danach.
3. Die angewandte Auswahl beim Backend zentralisieren; lokale Entwürfe bleiben
   in der Shell. Keine doppelte Persistenz oder konkurrierenden Wahrheiten.
4. Audioerfassung ohne aktive Session/Test beenden; Hintergrund-Discovery bleibt.
5. Keine Support-/Synchronitätsversprechen ohne passende Hardwaremessung.

Die kompakte Übersicht ist mit `1ac5c31` implementiert; der freigegebene
Fünf-Punkte-Plan hat Gruppen, Quellen und Pegel integriert. Der nächste Schritt
ist die unabhängige Abschlussprüfung und die in der Releaseprüfung benannte
native/physische Abnahme, nicht eine erneute Implementierung derselben Seiten.
Die übrigen Teilprojekte behalten ihre verbindlichen Ziele und Gates aus
diesem Gesamtplan; ihre Codepläne werden auf die dann tatsächlich vorhandenen
Schnittstellen geschrieben, nicht auf hypothetischen künftigen Code.

## 10. Quellen im Repository

- `docs/REPOSITORY_AUDIT_2026-09-08.md`: aktuelle Fixes und Testnachweise.
- `docs/UI_WINDOWS_NATIVE_2026-09-08.md`: aktuelle Komponenten-/Tokenregeln.
- `docs/superpowers/specs/2026-08-24-openaircast-windows-native-redesign-design.md`:
  genehmigte Grundgestaltung, State-/Accessibility-Verträge.
- `docs/superpowers/specs/2026-08-22-openaircast-device-resilience-design.md` und
  `2026-08-22-openaircast-diagnostics-calibration-design.md`: Domänengrenzen.
- `docs/VALIDATION.md`: historische Hardwarebefunde samt Grenzen.
- Nutzer-Screenshots vom 2026-09-08: tatsächlicher Dark/DE-Zustand aller sechs
  Seiten; nicht als öffentliche Repository-Assets übernommen.
