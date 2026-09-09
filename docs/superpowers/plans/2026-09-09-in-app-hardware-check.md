# Hardwareprüfung aus OpenAirCast

## Abschlussstand — 9. September 2026

- [x] Task 1: reale Sitzungen mit der Diagnose-Registry verbunden.
- [x] Task 2: manueller Assistent, Quellen-/Sitzungsbindung, Bericht und UI-Tests.
- [x] Task 3: validierte lokale Pufferkapazität bis zum AudioBuffer verbunden.
- [x] Task 4: Reviews, vollständige Testsuite, gezielte Nachprüfung des letzten
  Fixes, vier visuelle Ansichten und neuer lokaler Entwicklungsbuild.

Buildpfad, Prüfnachweise und Bedienung: [Hardwareprüfung](../../HARDWARE_CHECK.md).
Keine reale Wiedergabe oder Veröffentlichung durchgeführt. Die gesonderten
Hardwarefreigaben und der bekannte Suspend-/Cancel-Fehler bleiben offen.

## Auftrag und Design

Eine Hardwareprüfung ist bisher nicht startbar. Audio erhält einen Einstieg
„Hardwareprüfung öffnen“, Diagnose einen geführten, ausdrücklich manuellen
Hörtest mit technischen Vorbedingungen. Kein stiller Audio-, Standby- oder
Netzwerktest. Die Windows-Native-Gestaltung bleibt erhalten: Segoe UI,
bestehende ThemeTokens (dunkel: #10151F Fläche, #18202D Karte, #F4F7FB Text,
#9BA7BA Sekundärtext, #7EA6FF Akzent), linksbündige
Beschriftungen, mindestens 40 px hohe Aktionen, sichtbarer Tastaturfokus.
Die Abfolge Vorbereitung → bewusster Wiedergabestart → Ergebnis ist wichtiger
als zusätzliche dekorative Kacheln. DE/EN und hoher Kontrast bleiben unterstützt.

## Grenzen und Sicherheit

- Nur die aktuelle bestätigte Empfängerauswahl testen; keine automatische Suche
  nach anderen Geräten, keine Mikrofonaufnahme und keine synthetischen Töne.
- Der Nutzer startet selbst Systemaudio; vor dem Übertragen steht eine
  ausdrückliche Wiedergabeaktion. Ein offener Assistent startet niemals Audio.
- Hörbestätigung ist Nutzerbeobachtung, kein gemessener akustischer Versatz.
- Ein lokaler Funktionstest ersetzt nicht die Freigabematrix für experimentelle
  Latenzprofile. Niedrig/Stabil bleiben gesperrt, solange deren Parameter und
  Hardware-Nachweise fehlen. Der bisher missverständliche Freigabehinweis wird
  präzisiert. Normal wird mit tatsächlicher Senderpuffer-Anbindung dokumentiert.
- Keine Änderungen an veröffentlichten Releases; neuer lokaler Build.
- Cargo seriell, BelowNormal, -j 2, offline/locked; keine reale Wiedergabe durch
  den Entwicklungsagenten. Quota-Stopp bei 15 % Rest.

## Task 1: Diagnose-Lebenszyklus verbinden

Registriere reale Single-/Group-Sitzungen bei der vorhandenen DiagnosticsRegistry.
Eine Registry bleibt die einzige Quelle. Alte/gestoppte Sitzungen dürfen keine
aktuellen Messwerte vortäuschen. Transport stellt einen Read-only-Source bereit;
der Backend-Lebenszyklus verbindet ihn generationstreu mit dem Registry-Besitzer.
Keine Raw-IDs oder Fehlertexte in neue UI-Daten übernehmen. Offline-Tests für
Registrierung, Ende und veraltete Sitzung; bestehende Privacy-Verträge erhalten.

## Task 2: Geführte Hardwareprüfung und Diagnose-Aktionen

Audio verlinkt zu Diagnose. Dort Vorbereitung, Voraussetzungen, gezielter
Wiedergabestart/-stopp und bewertbares Ergebnis für die aktuelle Auswahl.
Ein Hörtest kann nur für tatsächlich aktive vollständige Empfänger und nach
Nutzerbestätigung als bestätigt gelten; Abbruch/Fehler/Änderung der Konfiguration
invalidieren die Beobachtung. Vorbedingungen enthalten niedrige Lautstärke,
Aufnahmebereitschaft, Auswahl und keine laufende fremde Sitzung.
Ein datensparsamer Bericht lässt sich explizit in die Zwischenablage kopieren
(keine Namen, IPs, MACs, Rohfehler oder Pfade). Berichte bleiben als Momentaufnahme
kenntlich; keine automatische Zertifizierung oder dauerhafte Freischaltung.
Automatisierte Zustands-, Interaktions-, DE/EN- und Datenschutztests.

### Umsetzungskontrakt

Ein kleines `app/hardware_check.rs` hält Testzustand und reine Prüfregeln;
AppState/UiSnapshot tragen Zustand bzw. sichere Projektion. AppEvent bekommt
typisierte Öffnen/Starten/Bestätigen/Abbrechen-Aktionen. Der vorhandene Reducer
bleibt Eigentümer der Start-/Stop-Effekte. Kein zweiter Streamingpfad.
Vorbereitung verlangt bestätigte vollständige Auswahl ohne ungespeicherten
Entwurf, bekannte Aufnahmebereitschaft, Normal-Profil, nicht stumm,
bestätigten Masterpegel >0 und <=15 %, keine offene Pegeländerung und keine
laufende Sitzung. Der Assistent bietet eine ausdrücklich geklickte Absenkung
auf 10 % und Verweise zu Auswahl/Audio. Diese Einstellung bleibt sichtbar;
keine automatische spätere Lautstärkeerhöhung.

Beim Start bindet sich der Versuch an Sitzungsgeneration, Auswahl, Audiogerät,
Profil und Pegel. Änderungen, Degraded/Restarting, Fehler oder ungefragter
Stopp verhindern eine positive Hörbestätigung. Erneutes Starten/Öffnen darf
keine laufende Sitzung übernehmen. Nach positiver oder negativer Bestätigung
wird gestoppt; erst passende Stopbestätigung macht den Versuch abgeschlossen.
Abbrechen bleibt auch im Verbindungsaufbau möglich. Der bekannte ausbleibende
Stop-Ack darf niemals als abgeschlossener Test dargestellt werden.
Ohne Mikrofonmessung kann der Nutzer nur bestätigen, dass alle ausgewählten
Lautsprecher hörbar spielten; Laufzeit, akustischer Versatz und Paketverlust
werden dabei nicht als automatisch geprüft behauptet.

Eine eigene `ui/pages/hardware_check.rs` zeichnet den Ablauf in Diagnose,
auch wenn noch keine Messwerte vorliegen. Die Übersicht der drei sicheren
Registry-Werte bleibt verfügbar, mit Erklärung: verlorene Diagnoseereignisse
sind keine verlorenen Audiopakete. Berichtkopie benutzt ausschließlich
lokalisierte feste Texte, Anzahl, Testzustand und sichere Registry-Werte;
keine Debug-Ausgabe des Zustands. Die Kopie ist ausdrücklich eine aktuelle
Momentaufnahme, keine dauerhafte Validierungsdatei.

## Task 3: Tatsächlichen Senderpuffer anbinden

Normal.sender_buffer_ms über Client/Streamer auf den echten lokalen AudioBuffer
anwenden, Default 2000 ms erhalten. Nicht auf RTSP latencyMin/Max oder Renderlead
abbilden. Regression für Unabhängigkeit der beiden Einstellungen. Keine neuen
experimentellen Zahlen als hardwarevalidiert deklarieren.

Der lokale Puffer wird als `SenderBufferCapacity` in `StreamConfig` durch die
vorhandenen Single-/Group-Pfade getragen. Ein privater Werttyp begrenzt die
allgemeine API auf 100..=10000 ms, Standard weiterhin 2000 ms. Dies sind
Ressourcengrenzen, keine Hardwarefreigabe. Der Backend-Adapter lehnt ungültige
Konfigurationen vor Connect ab. Der Streamer verwendet den Wert für seinen
tatsächlichen AudioBuffer; RTSP-Plists bleiben davon unberührt.

## Task 4: Prüfung und Build

Task-Reviews und abschließende risikobasierte Review, Formatcheck, fokussierte
Tests und relevante UI-Suite. App-Dateisperre respektieren. Neuen Build notfalls
als getrenntes Ausgabeziel bereitstellen, ohne laufende App oder alten Release
zu ersetzen. Anleitung mit Klickpfad, Grenzen und noch offenen Hardwaretests
aktualisieren. Reale Hardwareprüfung bleibt vom Nutzer auszuführen.
