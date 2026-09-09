# Hardwareprüfung und Diagnose

Download, Installation und lokale Build-Ordner sind in der [README](../README.md)
und der [Distributionsanleitung](DISTRIBUTION.md) beschrieben. Die Build-Nachweise
weiter unten sind lokale Entwicklungsnachweise, keine veröffentlichten Downloads.

Live-Diagnose für Windows-Eingang und Audioübertragung sowie ein geführter
lokaler Funktionstest mit einem selbst erzeugten Testton.
Der Assistent prüft technische Voraussetzungen; das tatsächliche Hören bestätigt
der Nutzer selbst. Der frühere reine Windows-Loopback-Test erzeugte keinen Ton
und wurde deshalb zu Recht als stumm gemeldet.

## Einstieg

Für die **Live-Diagnose** reicht eine normale Übertragung: Musik oder ein Video
auf dem ausgewählten Windows-Gerät abspielen, OpenAirCast-Streaming starten und
**Diagnose → Detaillierte Zähler anzeigen** öffnen. Dafür ist kein Hörtest nötig.
Die folgenden Schritte betreffen nur den getrennten Testton-Assistenten.

1. Unter **Übersicht** die gewünschten Lautsprecher auswählen und Änderungen übernehmen.
2. Unter **Audio → Hardwareprüfung öffnen** den Assistenten in **Diagnose** öffnen.
3. Voraussetzungen prüfen: Windows-Audio bereit, Normal-Profil, nicht stumm,
   bestätigte Masterlautstärke über 0 und höchstens 15 %, Einzelpegel über 0.
   **Masterlautstärke auf 10 Prozent setzen** stellt den niedrigen Masterpegel
   ein und erhöht später nicht automatisch zurück. Die Bestätigung der
   Änderung abwarten; anschließend **Auswahl und niedrige Lautstärke geprüft**
   ankreuzen. Eine andere laufende Übertragung zuerst stoppen.
4. **Hörtest mit Testton starten** wählen. Erst nach vollständigem Verbindungsaufbau
   wird einmalig ein etwa zwei Sekunden langer Testton erzeugt und durch den
   gemeinsamen Audio-/Encoder-/Senderpfad übertragen. Musik, Videos und der
   Testknopf in den Windows-Soundeinstellungen sind dafür nicht erforderlich.
   Öffnen, Hilfe und Navigieren starten keinen Ton.
5. Prüfen, ob alle Lautsprecher hörbar spielen. Dann
   **Alle ausgewählten Boxen gehört – stoppen** oder **Nicht alle Boxen gehört –
   stoppen** betätigen; alternativ **Abbrechen**. Die App beendet ihre Testsitzung.
   Anschließend **Diagnose-Momentaufnahme kopieren** wählen. Der Testton selbst
   endet automatisch; danach wird wieder Windows-Audio übertragen, bis die
   Testsitzung ausdrücklich beendet wird. Empfangspuffer können den Ton verzögern.

## Was das Ergebnis bedeutet

Die Hörbestätigung stammt vom Nutzer. OpenAirCast misst dabei weder den Schall im
Raum noch einen akustischen Zeitversatz. Die technische Voraussetzung ist, dass die
vollständige Auswahl in dieser Testsitzung aktiv ist. Änderungen an Auswahl,
Audiogerät, Pegel oder Profil sowie Verbindungsfehler machen einen begonnenen Test
ungültig. Das gilt auch für einen Wechsel des tatsächlich aufgenommenen Geräts bei
unveränderter Auswahl „Windows-Standard“, selbst bei gleichen Anzeigenamen.
Eine ausstehende Stopp-Bestätigung ist kein erfolgreich beendeter Test.

Der Bericht enthält nur Zustände und Zählwerte, keine Lautsprechernamen,
Netzwerkadressen, Hardwarekennungen, Dateipfade oder Rohfehlermeldungen. Er wird
nur nach Klick kopiert und nicht automatisch hochgeladen. Zum dauerhaften
Aufbewahren in eine eigene Textdatei einfügen.

## Latenzprofile

**Normal** ist das verfügbare Profil. Der lokale Senderpuffer hat eine Kapazität
von 2000 ms; der Render-Vorlauf beträgt 200 ms. Das sind Konfigurationswerte,
keine gemessene Ende-zu-Ende-Latenz und keine garantierte Zeit bis zum hörbaren Ton.

**Niedrig** und **Stabil** benötigen weiterhin definierte und freigegebene
Parameter sowie eine Entwickler-Hardwarematrix. Ein einzelner lokaler Hörtest
schaltet sie nicht frei. Das frühere „Erst nach der Hardwareprüfung wählbar“
war missverständlich: Es existierte kein entsprechender Startknopf.

## Diagnosewerte richtig lesen

Die Live-Diagnose ergänzt den Hörtest. Der Testton prüft den Weg ab dem
gemeinsamen PCM-Senderpfad; der Windows-Eingangspegel wird dagegen direkt an
den tatsächlich aufgenommenen Windows-Samples gemessen. Der erzeugte Testton
und das Kalibrierungssignal gehen nicht in diesen Eingangspegel ein.

- **Eingangssignal:** Spitzenwert des jüngsten Messfensters, als Prozent des
  digitalen Vollpegels. Das ist weder die Windows-Lautstärkeeinstellung noch
  eine Messung der Lautstärke im Raum. Ein gemessener Nullpegel und eine
  fehlende/veraltete Messung sind unterschiedliche Zustände.
- **Senderpuffer:** aktuell belegte und verfügbare Frames, gepufferte Audiozeit
  sowie Unterläufe seit Sitzungsstart. Die Pufferzeit ist keine gemessene
  Ende-zu-Ende-Latenz.
- **Übertragung je Lautsprecher:** Audio-Pakete und Bytes, die das lokale
  Betriebssystem zum Senden angenommen hat, lokale Sendefehler und angeforderte
  Wiederholungs-Slots. Erfolgreiches lokales Senden beweist keinen Empfang oder
  hörbaren Ton. Wiederholungsanforderungen sind kein Paketverlustzähler.
- **Verworfene PCM-Frames:** kumulierte Verdrängungen an der PCM-Brücke seit
  App-Start, nicht Netzwerkverluste und kein Zähler sämtlicher Windows-Probleme.

Sitzungswerte werden nur angezeigt, wenn die aktuelle Backend-Generation und
die aktive Diagnose-Registrierung zusammenpassen. Beendete Sitzungen, alte
Empfängerzeilen und veraltete Registry-Snapshots liefern keine aktuellen Zahlen.
Der Eingang wird in kurzen Fenstern abgetastet; ein mehr als eine Sekunde alter
Sample-Messwert wird beim Abholen verworfen. Bleiben Veröffentlichungen der
Diagnose-Registry zwei Sekunden aus, werden deren Sitzungswerte ausgeblendet.
Lautsprechernamen stammen weiterhin nur aus der bestehenden sicheren Geräteliste;
freie Fehlertexte, Netzwerkadressen und Windows-Gerätekennungen werden nicht
in die neue Datenansicht übernommen.

**Hinweise richtig verwenden:** Ein frischer Nullpegel legt nahe, zuerst die
Windows-Quelle zu prüfen. Aktuelle Verbindungszustände helfen beim Eingrenzen
einer nicht erreichbaren Box. Kumulierte Fehler werden mit ihrem jeweiligen
Zeitraum beschrieben: PCM-Verdrängungen seit App-Start, Pufferunterläufe und
Sendefehler seit Sitzungsstart; nicht automatisch als fortbestehende Störung. Die
Diagnose verändert keine Einstellungen und startet keine Wiedergabe.

Die ausführlichen Zähler lassen sich bei Bedarf einblenden. Hilfe, Zurück und
Abbrechen während eines laufenden Hörtests bleiben in der obersten Zeile.
Niedrig/Stabil bleiben gesperrt: Auch diese Werte sind keine Hardwarefreigabe.

### Bisherige Zusammenfassung

- **Gesamtzustand:** konservative Bewertung der aktuellen registrierten Sitzung.
- **Verlorene Diagnoseereignisse:** Überlauf der internen Diagnosemeldungen,
  ausdrücklich kein Zähler verlorener Audiopakete.
- **Empfänger mit Messwerten:** Anzahl der Empfänger mit registrierten Messungen,
  nicht bloß die Zahl gefundener oder ausgewählter Lautsprecher.

„Unbekannt“ bleibt korrekt, solange eine Messung fehlt. Ein Stopp oder ein neuer
Verbindungsversuch darf frühere Werte nicht zu aktuellen Messungen machen.
Ohne aktive Sitzung zeigt der aktuelle Empfängerzähler deshalb 0; intern
aufbewahrte Diagnosehistorie wird nicht als laufende Messung gezählt.

## Gestaltung und Bedienung

Der Assistent nutzt die vorhandene Windows-Native-Oberfläche mit Segoe UI,
ThemeTokens und linksbündigen Beschriftungen. Die Audio-Geräteauswahl bleibt
ebenfalls linksbündig. Diagnose enthält den Prüfablauf auch ohne Messwerte;
vorhandene Messwertkacheln stehen getrennt oberhalb des Hörtests.

Vorbereitung, Verbindung, Hören, ausstehender Stopp und Ergebnis sind getrennte
Zustände. Schaltflächen haben mindestens 40 px Höhe und sichtbaren Tastaturfokus.
Deutsch, Englisch und hoher Kontrast werden unterstützt. Der Start ist gesperrt,
solange Voraussetzungen fehlen; Bestätigung ist erst für die vollständige aktive
Auswahl möglich. Erfolg wird weder durch Farbe allein noch durch einen bloßen
Verbindungsaufbau angezeigt.

In der ersten Zeile stehen **Zurück zu Audio** und **Hilfe anzeigen**; während
Verbindungsaufbau und Hörphase zusätzlich **Abbrechen**, auch im kleinen Fenster
direkt sichtbar. Zurück führt direkt zu den Audio-Einstellungen. Dieser
Knopf navigiert nur; einen laufenden Hörtest beendet man ausdrücklich mit
**Abbrechen**. Neben dem gesperrten Start steht der konkrete nächste Schritt,
etwa Auswahl übernehmen, Lautstärkebestätigung abwarten oder Streaming stoppen.
Bei einer bereits laufenden Übertragung bietet der Assistent **Streaming stoppen**
an. Danach startet kein Hörtest automatisch; ein weiterer bewusster Klick ist nötig.

Die Hilfe öffnet die ausführlichen Voraussetzungen und Grenzen direkt unter
dieser ersten Zeile; sie ist anfangs eingeklappt. Darunter folgen ein konkreter
Diagnosehinweis, Windows-Audioquelle, Aufnahmezustand und Eingangssignal. Die
ausführlichen Zähler sind separat einklappbar. Sitzungsstatus und bisherige
Zusammenfassung folgen vor dem getrennten Hörtest. Status und Quelle bleiben
auch ohne aktive Sitzung sichtbar.
Der aktuelle Startblocker und die Erklärung des Testtons sind unabhängig von der
Hilfe sichtbar. „Nimmt auf“ beschreibt den Aufnahmezustand, keinen gemessenen
Signalpegel. „Läuft“ beschreibt eine Sitzung, keinen Nachweis hörbaren Schalls.

## Technischer Testtonpfad

Der Startklick autorisiert genau einen Ton in dieser Sitzung. Die Oberfläche
wartet auf alle ausgewählten verbundenen Empfänger; doppelte Statusmeldungen
lösen keinen weiteren Ton aus. Der Backend-Befehl bindet die tatsächliche
Backend-Sitzung, Auswahl, Lautstärken und Audioquelle. Eine veraltete Konfiguration,
eine volle Warteschlange oder eine verlorene Befehlsbestätigung darf nicht als
erfolgreicher Tontest gelten.

Der separate, begrenzte PCM-Testpfad erzeugt Stereo mit 44,1 kHz, 660 Hz,
maximal 8192 von 32767 und weichen Ein-/Ausblendungen. Der bestehende niedrige
Masterpegel bleibt zusätzlich wirksam. PCM-Frames werden zeitlich dosiert statt
als kompletter Ton in den Verdrängungspuffer geschrieben. Währenddessen wird
Windows-Audio verworfen, danach wieder normal weitergeleitet. Stopp, geänderte
Konfiguration oder das Ende des Controllers brechen weitere Testtonerzeugung ab;
bereits übertragene Empfängerpuffer sind davon zu unterscheiden.

## Noch gesondert zu prüfen

Standby/Aufwachen, echte Netzwerkunterbrechung, längere Stabilitätsläufe,
akustische Synchronität und andere Hardware-/Firmwarekombinationen. Diese
Aktionen führt der Assistent nicht automatisch aus. Der bekannte
Suspend-/Cancel-Stoppfehler ist durch einen Hörtest nicht behoben.

## Entwicklungsprüfung

Die Oberfläche wurde ohne echte Wiedergabe mit synthetischen Daten geprüft:
Vorbereitung und Hörphase in Deutsch/Englisch, hell/dunkel sowie bei 900 × 600
und 1120 × 720. Die vier gerenderten Ansichten wurden visuell kontrolliert.
Die fokussierten Hardwarecheck-Tests prüfen außerdem bewussten Start,
vollständige Auswahl, Abbruch, Quellenwechsel, Sitzungsgenerationen und die
Bestätigung des Stopps. Dies ersetzt keinen Hörtest an den eigenen Lautsprechern.

Die Live-Diagnose wurde zusätzlich mit fünf synthetischen Ansichten geprüft:
Ruhezustand, laufende Übertragung und Wiederverbindung sowie die gescrollten
Lautsprecherzähler. Die Prüfung umfasst ein kleines kontrastreiches Fenster und
eine breite helle Ansicht. Die dargestellten Beispielzahlen sind keine Messungen
an realen Lautsprechern. Die Hilfe steht weiterhin zuerst; der Hörtest besitzt
weiterhin den oberen Abbruchknopf.

Die vollständige Paketprüfung zur Live-Diagnose besteht mit 1.118 Tests:
163 Bibliotheks-, 780 App- und 175 Integrationstests. Sechs Tests sind regulär
ignoriert: zwei echte Windows-Fenster-/Hotkey-Prüfungen und vier manuelle
Bildprüfungen. Die Bildprüfungen für Hardwarecheck und Live-Diagnose wurden
separat erfolgreich ausgeführt. Ein unabhängiges Review prüfte die Projektion,
die Eingangspegelmessung und die Diagnosehinweise. Dabei gefundene Probleme bei
der Reihenfolge kumulierter PCM-Zähler und fehlenden Quellmesswerten sind mit
zunächst fehlschlagenden und anschließend erfolgreichen Regressionstests behoben.

## Lokaler Entwicklungsbuild — Live-Diagnose, 9. September 2026

Datei: `target/x86_64-pc-windows-msvc/debug/openaircast.exe`.
Windows x64, Entwicklungsprofil (nicht optimiert), nicht signiert.
Kein automatischer App-Start, keine echte Lautsprecherwiedergabe und keine
Hardwareunterbrechung. GitHub-Release und `dist/` bleiben unverändert.

- Anwendungsquellstand: `b7d1fbf65b8ac7e0675d7a787ded1a88ece605ad`.
- Build erfolgreich, Exit 0, 46,80 s; Dateigröße 65.023.488 Byte.
- Erstellt am 9. September 2026 um 16:51:55 UTC.
- SHA-256: `23EF33D9B52B9CE205FD215C2C6BE4D2C38824F6ACB271D07B0EBB7D67DFC7E5`.
- Vollständige Paketprüfung: 1.118 bestanden, 0 fehlgeschlagen, 6 ignoriert.
  Beide Diagnose-Bildprüfungen separat bestanden; insgesamt neun Ansichten
  einschließlich der gescrollten Lautsprecherzähler visuell kontrolliert.
- Clippy für Bibliothek und App mit `--no-deps`: Exit 0. Sechs bestehende
  Hinweise in der App und vorhandene Compilerwarnungen in Abhängigkeiten bleiben;
  keine Warnung verweist auf die neu hinzugefügte Diagnoseimplementierung.
  Geänderte Rust-Dateien bestehen den gezielten rustfmt-Check, der Diff die
  Whitespace-Prüfung. Unabhängiges Review ohne verbliebenen wichtigen Befund.

Die Eingangspegelmessung, Sitzungszuordnung und Fehlerfälle sind automatisiert
mit synthetischen Daten abgesichert. Der Praxisschritt bleibt: normale
Windows-Audioübertragung starten und unter Diagnose die detaillierten Zähler
ansehen. Der früher vom Nutzer gehörte Testton bestätigt nicht automatisch die
neuen Live-Messwerte. Akustische Latenz, Synchronität und tatsächlicher
Paketempfang werden damit weiterhin nicht gemessen.

## Vorheriger lokaler Build — echter Testton, 9. September 2026

Datei: `target/x86_64-pc-windows-msvc/debug/openaircast.exe`.
Windows x64, Entwicklungsprofil (nicht optimiert), nicht signiert.
Die App wurde nicht automatisch gestartet; keine echte Wiedergabe oder
Hardwareunterbrechung wurde ausgelöst. GitHub-Release und `dist/` bleiben unverändert.

- Anwendungsquellstand: `4e01bcddcb2e4ca6c3056ba248015d2f752f7b28`.
- Build erfolgreich, Exit 0, 45,77 s; Dateigröße 64.945.664 Byte.
- Erstellt am 9. September 2026 um 14:33:20 UTC.
- SHA-256: `725984AC2A6F7AA7EC71F5F53A2DC71057FD49536C31F4DBA3F421D1CE91EDD7`.
- Vollständige Prüfung des Pakets `homepod-cast`: 1.094 bestanden,
  0 fehlgeschlagen, 5 ignoriert. Davon 153 Bibliotheks-, 766 App- und
  175 Integrationstests; zusätzlich ein leeres Doctest-Ziel.
  Die ignorierten Tests betreffen echte Windows-Fenster/Hotkeys sowie drei
  manuelle Bildprüfungen. Die Diagnose-Bildprüfung wurde separat ausgeführt:
  1 bestanden, vier Ansichten in Deutsch/Englisch, hell/dunkel, klein/breit
  visuell kontrolliert.
- Gezielt abgesichert: tatsächliche, begrenzte PCM-Ausgabe auch über einen
  angeschlossenen Sitzungsdecoder ohne Lesen des Referenzdecoders; dosierte
  Ausgabe ohne Pufferflutung; Abbruch; Verwerfen doppelter und veralteter
  Tonanforderungen; korrekte Zuordnung der unterschiedlichen Sitzungsgenerationen;
  echter Sitzungsstopp mit Bestätigung bei fehlgeschlagenem Tonstart.
- Unabhängiges Backend- und UI-Review ohne verbliebene Befunde.
  Geänderter Rust-Code besteht den gezielten rustfmt-Check, der Diff die
  Whitespace-Prüfung. Bestehende Warnungen in Abhängigkeiten bleiben bestehen.

Die vollständige Paketprüfung baute auch die echte App außerhalb der
Testkonfiguration. Dadurch wurde eine versehentlich nur für Tests verfügbare
Sitzungsabfrage erkannt und vor dem erfolgreichen Gesamtlauf korrigiert.
Anschließend wurden zwei Testblöcke ausschließlich formatiert; der finale Build
enthält diese Formatierung. Ein tatsächlicher hörbarer Erfolg an HomePods ist
durch diese automatisierten Prüfungen nicht belegt. Der Nutzer hat den Testton
anschließend als hörbar bestätigt. Das ist ein Funktionserfolg, weiterhin keine
akustische Latenz- oder Synchronitätsmessung.

```powershell
(Get-Process -Id $PID).PriorityClass = 'BelowNormal'
cargo test -p homepod-cast --offline --locked --target x86_64-pc-windows-msvc -j2 -- --test-threads=2
cargo test -p homepod-cast --bin openaircast render_hardware_check_visual_review --offline --locked --target x86_64-pc-windows-msvc -j2 -- --ignored --test-threads=1
cargo build -p homepod-cast --bin openaircast --offline --locked --target x86_64-pc-windows-msvc -j2
```

## Vorheriger lokaler Build — Bedienungsverbesserungen, 9. September 2026

Datei: `target/x86_64-pc-windows-msvc/debug/openaircast.exe`.
Windows x64, Entwicklungsprofil (nicht optimiert), nicht signiert.
Keine automatische Ausführung, keine echte Wiedergabe und kein neuer GitHub-Release.

- Anwendungsquellstand: `07e1f2f4c5c5a1c5cbe37f4ad00d271e88c33b6b`.
- Build erfolgreich, Exit 0, 23,79 s; Dateigröße 64.866.816 Byte.
- Erstellt am 9. September 2026 um 13:21:07 UTC.
- SHA-256: `2D5379C4DBB7FF35C293F1603A9E73672CA25EEE35407CE7DBE90271E473BC6C`.
- Hardwarecheck-Prüfung: 14 bestanden, 1 visueller Test zunächst ignoriert.
  Der visuelle Test wurde separat erfolgreich ausgeführt; alle vier aktuellen
  Ansichten wurden kontrolliert. Ein nicht darstellbares Pfeilzeichen wurde
  durch den eindeutigen Textknopf „Zurück zu Audio“ ersetzt.
- Sprachprüfung: 29 bestanden. Breite UI-Prüfung: 399 bestanden, 3 ignoriert,
  1 mehrdeutige Testsuche. Diese Suche wurde auf den vollständigen Statustext
  präzisiert und anschließend erfolgreich einzeln wiederholt. Der gesamte
  UI-Lauf wurde nach dieser reinen Testkorrektur nicht erneut ausgeführt.
- Unabhängiges Code-Review ohne kritische oder wichtige Befunde. Kleine offene
  Testergänzung: Zurück-Navigation zusätzlich während der Hörphase durch den
  Reducer prüfen; die aktuelle Navigation erzeugt keinen Audiobefehl.

Bedienung und Grenzen sind oben dokumentiert. Die vollständige Workspace-Prüfung
unten gehört ausdrücklich zum vorherigen Stand. Bekannte Protokoll-/Stoppfehler
und die fehlende Freigabe der Profile Niedrig/Stabil wurden nicht verändert.

## Vorheriger lokaler Build — 9. September 2026

Datei: `target/x86_64-pc-windows-msvc/debug/openaircast.exe`.
Windows x64, Entwicklungsprofil (nicht optimiert), nicht signiert.
Der veröffentlichte Release und die Dateien unter `dist/` wurden nicht ersetzt.
Die App wurde nicht automatisch gestartet.

- Anwendungsquellstand: `d7c904224e179a499bafcdd1fa091d5c954affc5`.
- Build erfolgreich, Exit 0, 1 min 09 s; Dateigröße 64.858.624 Byte.
- SHA-256: `A6FEFE60A688D2CCA662D00734581EFEFDAE52BADD105A3B46D56907A8C6FF2F`.
- Vollständige Workspace-Prüfung am Stand `7ecd25c`: 2.125 bestanden,
  0 fehlgeschlagen, 16 ignoriert, 44 Testziele einschließlich Doctests.
  Nach dem letzten Anzeige-Fix zusätzlich alle 131 Backend-Bridge-Tests
  bestanden. Beide Review-Freigaben (Anforderungen und Codequalität) liegen vor.

Die erfolgreichen Test-/Build-Läufe nutzten Offline/Locked, Windows-MSVC,
maximal zwei Compiler-Jobs, zwei Testthreads und BelowNormal-Priorität.
Bestehende Compiler-/Clippy-Warnungen und allgemeine Formatierungsaltlasten
bleiben bestehen. Die neuen Assistenten-Dateien bestehen den gezielten
rustfmt-Check; der Git-Diff besteht die Whitespace-Prüfung.

```powershell
(Get-Process -Id $PID).PriorityClass = 'BelowNormal'
cargo test --offline --locked --target x86_64-pc-windows-msvc --workspace -j 2 -- --test-threads=2
cargo test --offline --locked --target x86_64-pc-windows-msvc -j 2 -p homepod-cast --bin openaircast backend_bridge::tests:: -- --test-threads=2
cargo build --offline --locked --target x86_64-pc-windows-msvc -j 2 -p homepod-cast --bin openaircast
```
