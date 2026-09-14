# Hardwareprüfung und Diagnose

Download, Installation und lokale Build-Ordner sind in der [README](../README.md)
und der [Distributionsanleitung](DISTRIBUTION.md) beschrieben. Lokale Build-Ordner sind keine veröffentlichten Downloads.

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

Aktueller Prüfstand und offene Aufgaben: [Status](STATUS.md).
