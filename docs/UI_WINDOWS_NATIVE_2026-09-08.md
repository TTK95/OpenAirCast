# Windows Native — verbindliche UI-Fortschreibung

> Freigabeentscheidung vom 2026-09-09: Der Eigentümer erlaubt Commit, Merge,
> Push und Release-Vorbereitung trotz des bekannten Standby-/Abbruchfehlers.
> Der Fehler ist akzeptiert, nicht behoben; frühere Merge-Sperren in diesem
> Dokument sind damit historisch. Native Sicht- und Accessibility-Prüfungen
> bleiben unbestätigt. Details: [Releasehinweise](RELEASE_NOTES_2026-09-09.md).

Stand: 2026-09-09. Gilt für die bestehende egui-Desktop-Anwendung, nicht für
einen Browser-Prototyp. Windows Native bezeichnet die Gestaltung; es ist keine
Migration auf WinUI. Diese Fortschreibung ergänzt die vorhandene Designspezifikation.

## Ziel und Hierarchie

- Ruhige, kompakte Windows-Anwendung mit einer klaren primären Aktion pro Kontext.
- Navigation, Seitenkopf, scrollender Inhalt und Änderungsleiste bleiben getrennt.
- Keine dekorativen Verläufe, Emoji-Navigation oder frei erfundenen Live-Messwerte.
- UI liest den autoritativen Snapshot und sendet typisierte Ereignisse. Rendering
  löst weder Verbindungen noch Speicherung aus.

## Raster und Navigation

- Unter 1000 logischen Punkten Fensterbreite: 64 Punkte breite Symbolleiste,
  16 Punkte Seitenrand. Ab 1000: 176 Punkte Navigation mit Text, 24 Punkte Rand.
- Inhalt maximal 920 Punkte breit; Empfängerkarten mindestens 420 Punkte,
  16 Punkte Spaltenabstand. Zielgrößen: 900 × 600 und 1120 × 720.
- Navigation scrollt nicht. Nur der Seiteninhalt scrollt. Die Änderungsleiste
  reserviert unten 72 Punkte und darf Inhalte nicht überdecken.
- Neu: 52 Punkte hoher Markenbereich plus 8 Punkte Abstand. Links ein
  Wellenformzeichen; „OpenAirCast“ nur bei ausgeklappter Navigation.
- Navigationsziele 44 Punkte hoch; Einstellungen mit 16 Punkten Abstand absetzen.
- Aktiver Eintrag: Akzentsymbol und 3 × 20 Punkte Auswahlindikator. Inaktive
  Symbole verwenden `ink_muted`. Statusfuß bleibt dem tatsächlichen Zustand treu.
- Symbole als Vektorgeometrie, ungefähr 22 Punkte groß und 1,75 Punkte Strich:
  Haus, Lautsprecher, Lautsprechergruppe, Wellenform, Diagnosekurve, Zahnrad.
  Gemeinsame optische Mitte verwenden; keine unterschiedlichen Icon-Pakete mischen.

## Typografie

Segoe UI Variable verwenden, wenn auf Windows verfügbar; Inter als gebündelter
Fallback. Messwerte verwenden IBM Plex Mono. Größen und Zeilenhöhen in Punkten:

| Rolle | Größe | Zeilenhöhe |
|---|---:|---:|
| Seitentitel | 28 | 34 |
| Hervorgehobener Titel | 20 | 26 |
| Abschnitt | 16 | 22 |
| Fließtext | 14 | 20 |
| Schaltfläche | 14 | 18 |
| Sekundärtext | 12 | 17 |
| Kleine Rubrik | 11 | 15 |
| Messwert | 12 | 16 |

Bei manuell gezeichneten Texten die vollständige `TypographyRole` inklusive
Zeilenhöhe in den `LayoutJob` übernehmen. Kartenhöhe aus der tatsächlich
umgebrochenen Galley berechnen, nicht aus einer festen Zeilenanzahl.

## Farben und Oberflächen

Bestehende semantische Tokens erhalten, niemals Komponenten lokal einfärben.

| Token | Hell | Dunkel |
|---|---|---|
| canvas | #F4F6FB | #10151F |
| surface | #FFFFFF | #18202D |
| surface_subtle | #E9EDF5 | #222C3A |
| ink | #172033 | #F4F7FB |
| ink_muted | #5C6678 | #9BA7BA |
| border | #DCE2ED | #2F3A49 |
| route / focus | #3168E8 | #7EA6FF |
| live | #157766 | #60D6C2 |
| warning | #A9570D | #FFB45C |
| fault | #B94B5B | #FF8B9A |

High Contrast verwendet Windows-Systemfarben, nicht diese Palette. Kartenradius
12, Bedienelementradius 8, Kartenpadding 16, Abschnittsabstand 16 Punkte.
Bewegung maximal 120 ms; bei deaktivierter Systemanimation 0 ms.

## Bedienung und Zustände

### Kompakte Route und gemeinsame Lautsprecherauswahl (2026-09-09)

- Die normale Routenkarte auf „Übersicht“ misst im echten egui-Fixture 108
  logische Punkte (innerhalb des erlaubten Bereichs 88--112): 16 Punkte
  Kartenrand, Status/Quelle mit mindestens einem echten Zielnamen, eine
  Zeile für ausgewählte und tatsächlich aktive Empfänger und ein 28-Punkte-
  Diagramm. Lange lokalisierte Texte dürfen die Karte nach unten erweitern;
  sie werden nicht abgeschnitten.
- Der Text nennt Phase, „Windows-Audio“, sichtbare Zielnamen sowie Auswahl und
  echte aktive Wiedergabe getrennt. Der ASCII-Verbinder `>` ist bewusst
  font-sicher. Das Diagramm wiederholt diese Fakten nur als Quadratquelle,
  Verbindung und runde Empfängerknoten. Es löst keine Befehle aus; die
  vollständige Route bleibt zugänglich angekündigt.
- „Übersicht“ und „Lautsprecher“ verwenden dieselben auswählbaren
  Empfängerkarten mit stabiler Geräte-ID und denselben globalen Auswahlentwurf.
  Beide Seiten verwenden ausschließlich die bereits von der Shell angeheftete
  Änderungsleiste zum Anwenden oder Verwerfen. Seitenwechsel erzeugen weder
  eine zweite Auswahl noch eine zweite Anwenden-Schaltfläche.
- Der Einzelpegel liegt als eine kompakte, mindestens 40 Punkte hohe Reihe
  unter der Karten-Auswahl: kurze Beschriftung, Track und Prozentwert. Der
  Hilfetext „Anteil an Gesamtlautstärke“ bleibt per Hover am Namen und Regler
  erreichbar, statt jede Karte um eine dauerhafte Zeile zu vergrößern. Klick,
  Tastaturfokus und Pfeiltasten am Regler senden ausschließlich einen
  Pegelbefehl für seine stabile Empfänger-ID und verändern keine Auswahl. In
  kleinen Fenstern scrollt nur der Seiteninhalt; Kopf, Stop/Start und
  Änderungsleiste bleiben erreichbar.
- Die Gesamtlautstärke verwendet dieselbe horizontale 40-Punkte-Anordnung
  aus Beschriftung, Track und Prozentwert. Damit bleibt ihr vollständiger
  Regler bei 1120 × 720 oberhalb des Fensterrands sichtbar.

### Einzelpegel je Lautsprecher (freigegeben 2026-09-08)

- Auf „Übersicht“ und „Lautsprecher“ erhält jeder entdeckte Empfänger einen
  Regler von 0 bis 100 Prozent. Er steht getrennt vom anklickbaren Auswahlbereich:
  Lautstärke ändern darf weder Auswahl noch Sitzung verändern.
- Der Wert ist ein Anteil an der Gesamtlautstärke, keine zusätzliche Verstärkung:
  Gesamt 40 % × Lautsprecher 50 % ergibt 20 % Ausgangspegel. Gesamtlautstärke
  und globale Stummschaltung behalten ihre bisherige Bedeutung.
- Standard für bisher unkonfigurierte Lautsprecher: 100 %. Einstellungen
  dauerhaft anhand stabiler Empfänger-ID speichern, niemals anhand Listenposition
  oder Anzeigename. Änderungen gelten live, ohne Streaming-Neustart.
- Erst nach der ersten Backend-Rückmeldung Regler anbieten. Angezeigte Werte
  stammen aus bestätigter Speicherung; bei vollem Befehlskanal oder fehlgeschlagenem
  Speichern keinen erfolgreich übernommenen Wert vortäuschen.
- Beschriftung und Hilfetext auf Deutsch/Englisch. Zugänglicher Name enthält den
  Lautsprechernamen, keine Netzwerkadresse. Prozentanzeige rechts mit reservierter
  Breite, derselbe sichtbare Track und Fokusrahmen wie bei Gesamtlautstärke.
- Tokens, Rundungen und Tastaturbedienung des bestehenden Windows-Native-Designs
  wiederverwenden. IDs bleiben bei Sortierung stabil; beim Beenden deaktivieren.
- Abnahme: Der Eigentümer hat am 2026-09-09 bestätigt, dass alle drei
  Empfänger spielen und der Einzelpegel funktioniert. Die quantitative
  Langzeit-/Skew-Messung bleibt ein separates Release-Gate; Offline-Tests
  ersetzen diese Messungen nicht.
- Software-Nachweis 2026-09-09: Effektive Startpegel werden im produktiven
  Transportpfad vor dem Audio-Start angewendet (`4ce6b7d`, `c4b8ebe`,
  `08d83c8`, `a5136e6`). Spätere bereits eingegangene Pegel gewinnen gegen
  ältere Startwerte. Die neue Reihenfolge ist deterministisch geprüft;
  eine frische physische Hörprobe bei Start und Wiederverbindung steht aus.

### Windows-Audioquellen (2026-09-09)

- Die Auswahl nennt Windows-Wiedergabegeräte für Loopback-Audio, keine
  Mikrofone. „Windows-Standard“ ist die gespeicherte Präferenz; der tatsächlich
  verwendete Endpunkt wird separat aus dem Backend gemeldet.
- Endpunktinventar ist vor der ersten Messung unbekannt. Erst eine erfolgreiche
  leere Messung bedeutet „keine Geräte“. Ein fehlgeschlagener Refresh behält
  die letzte bestätigte Liste und zeigt eine lokalisierte Fehlermeldung; ein
  erfolgreicher Scan entfernt sie. Ein beendeter Scanner bleibt nicht still
  als endlos unbekannte Liste stehen.
- Produktive Enumeration läuft begrenzt außerhalb von UI und Controller.
  Ein hängender Scan blockiert weder Befehle noch Beenden und erzeugt keine
  beliebig vielen Ersatzworker. Die bestehende Endpunktauswahl bleibt bei
  einem Refreshfehler nutzbar.
- Stabile opake Endpunktschlüssel erhalten Auswahl und Tastaturfokus bei
  Sortierung; Namen und Listenposition sind keine Identität. Eine gespeicherte
  fehlende Quelle bleibt sichtbar, während bewiesene aktive Erfassung stärker
  zählt als ein unvollständiger Scan. Änderungen gelten erst mit Backend-Echo.
- Bounded-Worker-, Bridge-, Reducer- und UI-Fälle sind automatisiert geprüft.
  Reale Auswahl, Defaultwechsel, Entfernen/Wiederkehr und Aufnahmequalität
  benötigen weiterhin die autorisierte native bzw. physische Abnahme.

### Gemeinsame Regeln

### Gespeicherte Gruppen (2026-09-09)

- Die Gruppenliste zeigt ausschließlich den bestätigten `saved_groups`-Snapshot. Vor der ersten Lesung steht nur „wird geladen“; eine bestätigte leere Liste bietet „Gruppe erstellen“.
- Der Gruppen-Editor ist ein seitenlokaler Entwurf. Name, Mitgliedschaft und Gruppenpegel verändern weder den globalen Lautsprecherentwurf noch die laufende Wiedergabe. Offline-Mitglieder bleiben mit gespeichertem Namen und Pegel sichtbar.
- „Übernehmen“ und „Übernehmen und starten“ sind getrennte, ruhige Aktionen. Die zweite bestätigt nur die angeforderte Wiedergabe, niemals hörbares Audio. Ein globaler Auswahlentwurf verlangt ausdrücklich Beibehalten oder Verwerfen vor dem Übernehmen einer Gruppe.
- Löschen verlangt die Bestätigung mit dem exakten Gruppennamen. Alle Gruppenaktionen sind bei einer ausstehenden globalen Gruppenoperation gesperrt. Terminale Rückmeldungen werden nur der von dieser Seite ausgelösten Request-ID zugeordnet; `ConfirmationLost` bleibt ausdrücklich unbestätigt und wird nicht automatisch wiederholt.
- Der Editor verwendet kompakte Mitgliedszeilen mit der bestehenden nativen Reglerdarstellung und Prozentwerten. Deren Änderungen bleiben im Entwurf; sie senden keine Live-Pegelbefehle. Eingabefeld und Regler bieten mindestens 40 Punkte Bedienhöhe, stabile Empfänger-IDs und Tastatursteuerung.
- Ein Speicherklick sperrt weitere Aktionen bereits bis zur Shell-Annahme und anschließend bis zur zugeordneten Rückmeldung. Ein voller oder geschlossener Shell-Kanal zeigt lokalisierte Rückmeldung und behält den Entwurf. Abbrechen bleibt nach einer Ablehnung möglich. Ein alter Erfolg oder ein Ergebnis der gemeinsamen Lautsprecherauswahl schließt keinen neuen Editor; auch ein Erfolg ohne zuvor gerenderten Pending-Frame wird zugeordnet.
- Fehler und unbestätigte Ergebnisse stehen oberhalb des Editors. Leerliste (900 × 600), Liste (1120 × 720), Editor (900 × 600) und tatsächlicher Editorfehler in Windows High Contrast (900 × 600) wurden mit synthetischen Daten gerendert und geprüft: `target/groups-empty.png`, `target/groups-list.png`, `target/groups-editor.png`, `target/groups-error-contrast.png`. Diese Bilder ersetzen keine native Fenster- oder Hardwareabnahme.
- Auf „Gruppen“ bleibt der zur tatsächlichen Sitzung gehörende Stop-/Abbrechen-Befehl im festen Seitenkopf sichtbar, auch während ausstehendem Speichern. Der Editor erzeugt keinen zweiten Start-Befehl. Ein bloßes erfolgreiches Gruppenkommando erzeugt keinen Wiedergabestatus; dieser stammt weiterhin aus der Sitzung.
- „Übernehmen und starten“ aus dem Leerlauf oder während eines noch laufenden
  Stop-Vorgangs erzeugt sofort den korrelierten
  Zustand „Startet“ und einen erreichbaren Abbrechen-Befehl, schon vor aktiver
  Audioübertragung. Eine Startgeneration begleitet den bestehenden Gruppenbefehl;
  ein gespeichertes Gruppenergebnis allein erzeugt niemals „Wiedergabe aktiv“.
  Bei Queue-/Speicherablehnung wird die tatsächliche Backendphase erneut
  angezeigt. Ein späterer Stop behält seine Generation und wird von verspäteten
  Gruppenresultaten nicht zurückgenommen. Innerhalb einer Befehlsrunde unterdrückt
  ein späterer Stop frühere Gruppenstarts; ein ausdrücklich danach angeforderter
  Gruppenstart darf wieder starten. Verweigerte Gruppenpegel starten auch bei
  bereits identischen Mitgliedern keine Sitzung.
- Die Bridge wartet für einen Gruppenstart auf dessen exakte Bestätigung samt
  Snapshot-Revisionsgrenze. Vorher bestätigen weder ein alter aktiver Stream
  noch ein alter Fehler die neue Startgeneration. Auch eine geänderte
  Backendphase muss zur noch unbeantworteten Generation passen: Der Abschluss
  eines vorherigen Stop-Vorgangs entfernt nicht den neuen Abbrechen-Befehl.
  Nach abgeschlossener Zuordnung werden spontane Status-/Recovery-Änderungen
  derselben Generation weiterhin angezeigt. Ein erfolgreicher Gruppenbeleg
  allein erzeugt keine aktive Wiedergabe.
- Die Seite behält genau eine bereits beobachtete Gruppenrückmeldung auch dann, wenn eine spätere gemeinsame Auswahlaktion die globale Rückmeldung ersetzt. Wird das Gruppenergebnis während eines Seitenwechsels ungesehen ersetzt, bleibt der Entwurf mit „Ergebnis nicht bestätigt. Gespeicherte Gruppen prüfen.“ erhalten; daraus wird weder eine Ablehnung noch eine automatische Wiederholung abgeleitet. Die Verfügbarkeit nennt ausdrücklich „2 von 3 verfügbar“ beziehungsweise „2 of 3 available“.

Einzelpegel-Verifikation (2026-09-08): zwölf GPU-Renderings des echten egui-Shells
mit synthetischen Empfängern in Hell/Dunkel/High Contrast, jeweils 1120 × 720 und
900 × 600, wurden erzeugt und visuell geprüft. Artefakte liegen lokal unter
`target/receiver-volume-*.png`; sie enthalten keine echten Gerätekennungen.
Tastatur, Identität nach Umordnung, Beenden, unbekannter Zustand und Rendering
ohne Befehle sind automatisiert geprüft. Die Eigentümerbestätigung deckt die
Hörprobe der Einzelpegel ab; der native Windows-Fenster-/DPI-Test bleibt offen.

- Bedienelemente mindestens 40 Punkte hoch. Fokusoutline 2 Punkte außerhalb des
  Elements zeichnen; nicht mit dessen eigenem Rechteck abschneiden. Übergeordnete
  Scroll-/Fensterclips bleiben wirksam.
- Lautstärke: runder Griff, gedämpfte Hintergrundschiene, Akzentfüllung bis zur
  Griffmitte. Prozenttext zeigt den bestätigten Snapshot-Wert. Tastaturinkremente
  und vorhandene Ereignisse unverändert lassen.
- Empfänger- und Audioquellen-Steuerelemente behalten ihre Identität über
  Sortierung und Geräteänderungen. Für Audioendpunkte explizite Child-Ui-ID aus
  dem opaken Endpoint-Key bilden; Listenposition ist keine Geräteidentität.
- Mute, Latenz und Audioquelle zeigen erst nach Backend-Bestätigung neue Werte.
  Bei Ablehnung bleibt der bestätigte Zustand sichtbar.
- Nach erfolgreicher Wiederverbindung nur die passende alte Session-Fehlermeldung
  entfernen. Andere Hinweise und spätere Generationen nicht überschreiben.
- Alle interaktiven Elemente benötigen zugängliche, lokalisierte Namen.
  Kompakte Navigation behält Namen und Tooltips auch ohne sichtbaren Text.

## Prüfung vor Designfreigabe

1. Automatisierte UI-Matrix für Deutsch/Englisch, Hell/Dunkel/High Contrast,
   beide Fenstergrößen und alle vorhandenen Seiten/Zustände ausführen.
2. Fokusoutline, umgebrochene Empfängernamen, Slider-Geometrie und Audioquellen-
   Reorder mit Regressionstests prüfen.
3. Native App visuell bei 100 % und 150 % Skalierung prüfen: Tab/Shift-Tab,
   Space/Enter, Größenänderung, Scrollen, Kontrast, Tray und lange Gerätenamen.
4. Echte Audiogerätewechsel sowie Start/Stop und Wiederverbindung mit Hardware
   prüfen. Headless-Tests ersetzen diese Freigabe nicht.

Historisch am 2026-09-08 wurde der native App-Start zunächst wegen fehlender App-Freigabe
abgelehnt; nach dem Neustart der Host-App mit aktualisierten Rechten gelang er.
Die Screenshot-Schnittstelle scheitert weiterhin mit `SetIsBorderRequired`
und Fehler `0x80004002`. Daher ist diese Änderung **nicht visuell im laufenden
Windows-Fenster abgenommen**; der damalige native Start ist bestätigt.
Den aktuellen Build, den begrenzten nativen UI-Check und die verbleibende
Matrix dokumentiert [die Releaseprüfung vom 2026-09-09](RELEASE_CHECK_2026-09-09.md).
Die damalige Debug-EXE zu `b639d30` startete bei diesem Check erfolgreich; vorher waren
Auto-Connect ausgeschaltet und kein OpenAirCast-Prozess aktiv. Beide aktuellen
Screenshotversuche scheiterten wieder mit `SetIsBorderRequired`/`0x80004002`.
Textbasierte UI Automation zeigte nur Fenster-/Titelleisten-Elemente, keine
Seitencontrols. Die native Sicht-, Navigations- und Accessibility-Abnahme bleibt
daher blockiert und ungetestet. Es gab weder blinde Eingaben noch Audio- oder
Einstellungsänderungen; die App blieb am Ende für den Eigentümer im Vordergrund.
Diese Startbestätigung gilt nur für den dokumentierten alten Hash, nicht für
die späteren Reviewfixes bis `047dfc6`. Die Gesamtbranch-Review und ihre einzige
Korrekturrunde sind durchgeführt: Alle drei ursprünglichen Befunde sind in
`047dfc6` geschlossen (Audiofreigabe, bestätigter Masterpegel, erneute Auswahl
von Offline-Mitgliedern). **Software NICHT merge-bereit:** Ein neuer Important-
Restbefund bleibt offen. Suspend während der vorbereiteten Aktivierung kann
nach Cleanup die Stopped-Meldung der Generation verlieren; Cancel vor Ende der
zwei Sekunden langen Resume-Beruhigung kann die Shell in Stopping festhalten.
Nächste Quellcodearbeit: Regression „Suspend während Aktivierung → Cancel vor
Resume-Settle“ zuerst rot nachweisen, Stopped nach begrenztem Cleanup erhalten,
danach fokussiert reviewen und neu verifizieren. Keine zweite Quellcode-
Korrekturrunde in diesem Abschluss; grüne Bestandstests ersetzen diesen Test
nicht. Ein erfolgreicher Gruppenbeleg bestätigt
nur Speicherung/Startabsicht: Erst eine passende Backend-Sitzungsgeneration
darf den neuen Start beantworten. Frühes Recovering bleibt abbrechbar.
Der historische Workspace-Lauf auf `3595057` hatte 2.092 bestandene Tests,
0 Fehler und 15 ignorierte Fälle; das ist keine aktuelle Verifikation und
ersetzt keine native Sichtprüfung. Aktuelle Softwarebelege stehen getrennt
in der verlinkten Releaseprüfung. Es gab keinen neuen nativen Start oder Audiotest.
Frische Verifikation von `047dfc6070cf42d8404aca779dd17c57e5af6358`:
**2.104 bestanden, 0 Fehler, 15 ignoriert**, 44 Workspace-Ziele einschließlich
Doctests; scoped Clippy und normaler Debug-Build jeweils Exit 0 mit bestehenden
Warnungen. SHA-256 der nicht gestarteten normalen Debug-EXE:
`55201E6BEB941BBE07F0957C5A23744DFE2D8DC7C0BD7142B069E0B5B71B12A6`.
Der Important-Restbefund bleibt trotz dieser Ergebnisse offen; Software weiterhin
nicht merge-bereit. Pfad, Größe, Zeitpunkt und Warnungszahlen stehen in der Releaseprüfung.
Die frühere EXE wurde für den laufenden
Prozess unter `target/x86_64-pc-windows-msvc/debug/openaircast-ui-check-b639d30.exe`
erhalten. Ein neuer Build am normalen Ausgabepfad aktualisiert diesen Prozess
nicht; der Eigentümer muss für den neuen Stand ausdrücklich beenden/neustarten.
