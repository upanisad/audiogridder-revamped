# AudioGridder revamped – Piano di lavoro

Oct 17, 2026 · @Mauro Longone

Il lavoro è diviso in 5 fasi e 31 step: ogni step è piccolo, si verifica da solo e si chiude prima di passare al successivo. Riferimenti: [report di analisi](https://claude.ai/code/artifact/b0dcaca7-2a9b-48b4-858c-32a0cba746b8) e [specifica del protocollo](https://claude.ai/code/artifact/e6fa75cc-e754-4f9a-ada4-1b3d5825993c).

## Come lavoriamo

- **Un step alla volta.** Claude lavora sul fork e consegna ogni step come commit o pull request, con una nota su cosa è cambiato e come provarlo.
- **Mauro approva.** Uno step si spunta solo quando la sua verifica è superata; gli step marcati "Mauro" sono prove sulle macchine reali.
- **I cancelli contano.** L'ultimo step di ogni fase è il cancello della roadmap: se non passa, si torna indietro invece di andare avanti.
- **Le decisioni nuove** finiscono nella specifica, così il documento resta la fonte di verità.

## Fase 1 · Baseline

Obiettivo: un fork che compila e suona esattamente come la 1.2.0, più le misure di partenza.

- [ ] **1.1 Fork su GitHub** · Mauro · Repository "audiogridder-revamped" sul tuo account, con Claude abilitato all'accesso. Verifica: Claude riesce a fare push di un commit di prova.
- [ ] **1.2 Build Linux** · Claude · Plugin e server compilati qui così come sono. Verifica: build pulita. Fatto il 17 ottobre 2026; i test originali non sono eseguibili (richiedono plugin di terze parti assenti dal repository).
- [ ] **1.3 Build automatiche** · Claude · GitHub Actions che compila per macOS, Windows e Linux a ogni commit. Verifica: i tre build verdi, binari scaricabili (gli installer arrivano con la release). Fatto il 17 ottobre 2026, in revisione nella pull request #1.
- [ ] **1.4 Misure di riferimento** · Mauro, con procedura scritta da Claude · In Cubase con la 1.2.0: latenza, dropout e CPU su cavo diretto e su switch. Verifica: tabella delle misure salvata nel report.
- [ ] **1.5 Cancello** · Mauro · La build del fork suona come la 1.2.0 nella stessa sessione di prova.

## Fase 2 · Rete in Rust

Obiettivo: il modulo `agnet` sostituisce tutto il layer di rete, prima testato da solo e poi integrato. Gli step da 2.1 a 2.7 non toccano AudioGridder.

- [ ] **2.1 Workspace Cargo** · Claude · I quattro crate vuoti, versione minima di Rust fissata, build nelle GitHub Actions. Verifica: build verde sui tre sistemi.
- [ ] **2.2 agnet-proto** · Claude · Frame, messaggi del catalogo, layout del frame audio. Verifica: test di andata e ritorno su ogni messaggio, fuzzing senza crash.
- [ ] **2.3 agnet-transport** · Claude · TCP e socket locali su tutti e tre i sistemi, TCP\_NODELAY, marcatura DSCP. Verifica: test di connessione su loopback e socket locale.
- [ ] **2.4 agnet-session, sessione** · Claude · Handshake, versioning, heartbeat, riconnessione con ripresa, errori tipizzati. Verifica: test con disconnessioni simulate.
- [ ] **2.5 agnet-session, audio** · Claude · Buffer circolari, jitter buffer adattivo, modalità Auto e Fissa, fade-out di emergenza, metriche. Verifica: nessuna allocazione nel percorso realtime.
- [ ] **2.6 Banco di prova di rete** · Claude · I cinque scenari della specifica con `tc netem`. Verifica: tutti i criteri di accettazione superati.
- [ ] **2.7 Strumento di prova da riga di comando** · Claude, poi Mauro · Un client e un server minimi che scambiano audio sintetico e stampano latenza, jitter e dropout. Verifica: Mauro lo lancia tra PC e muletto, su cavo e su switch, prima di ogni integrazione.
- [ ] **2.8 Ponte C++** · Claude · agnet-ffi con `cxx` e integrazione nella build CMake tramite Corrosion. Verifica: AudioGridder compila con il modulo collegato ma ancora inutilizzato.
- [ ] **2.9 Integrazione nel plugin** · Claude · Client e streamer audio passano ad `agnet`. Verifica: build e test verdi.
- [ ] **2.10 Integrazione nel server** · Claude · Server principale, worker e intervallo di porte passano ad `agnet`. Verifica: plugin e server nuovi si parlano in locale.
- [ ] **2.11 Cancello** · Mauro · Prova in Cubase su cavo diretto e su switch: rete stabile, latenza pari o migliore delle misure 1.4.

## Fase 3 · JUCE 9 ufficiale

Obiettivo: abbandonare il fork JUCE dell'autore e passare alla versione ufficiale.

- [ ] **3.1 Launcher dei processi** · Claude · Sostituisce le patch ai processi figli (argomenti custom, timeout di ping, niente zombie) con codice proprio di AudioGridder. Verifica: sandbox per catena e per plugin funzionanti, nessun processo orfano.
- [ ] **3.2 Passaggio a JUCE 9** · Claude · Submodule su JUCE ufficiale 9.0.x, correzioni di compilazione, rimozione del supporto ai vecchi macOS. Verifica: build verde sui tre sistemi.
- [ ] **3.3 Dipendenze aggiornate** · Claude · vcpkg, FFmpeg, Boost alle versioni correnti; valutare il WebP nativo di JUCE 9 al posto di libwebp. Verifica: build e test verdi.
- [ ] **3.4 Cancello** · Mauro · Prova in Cubase su Windows 11, più macOS 26 con Logic. Serve un Mac per la prova: tuo o di un tester.

## Fase 4 · Linux e sandbox

Obiettivo: il server Linux, oggi sperimentale, diventa una piattaforma di prima classe.

- [ ] **4.1 Scansione dei plugin** · Claude · Risolvere il blocco su "Loading plugin layouts" e i crash segnalati. Verifica: scansione completa di una cartella di plugin VST3 Linux.
- [ ] **4.2 Thread realtime su Linux** · Claude · Priorità realtime con ripiego pulito quando il sistema la nega. Verifica: audio regolare sia con sia senza permessi realtime.
- [ ] **4.3 Pacchetti Linux** · Claude · Pacchetti installabili per le distribuzioni principali. Verifica: installazione pulita su Ubuntu 24.04 e Fedora.
- [ ] **4.4 Cancello** · Mauro · Una sessione reale con il server su un muletto Linux.

## Fase 5 · Miglioramenti e pubblicazione

Obiettivo: le funzioni nuove e la prima release pubblica. L'ordine degli step qui è flessibile.

- [ ] **5.1 Pannello di telemetria** · Claude · Nel plugin: latenza reale, jitter, buffer in uso, dropout. Verifica: valori coerenti con lo strumento di prova 2.7.
- [ ] **5.2 Codice di abbinamento** · Claude, se approvato · PIN facoltativo, spento di default, verificato nell'handshake. Verifica: connessione rifiutata con PIN errato, nessun effetto sulla latenza.
- [ ] **5.3 Trasporto UDP** · Claude · UDP con ridondanza dei pacchetti audio, scelto dall'utente. Verifica: scenari di rete ripetuti, meno dropout del TCP sulla rete con perdite.
- [ ] **5.4 UI del plugin in WebView** · Claude con Mauro · Nuova interfaccia del plugin con tecnologie web, sfruttando JUCE 9. Verifica: tutte le funzioni dell'interfaccia attuale presenti.
- [ ] **5.5 MIDI 2.0** · Claude · Trasporto dei messaggi MIDI 2.0 dove JUCE e le DAW lo supportano. Verifica: messaggi MIDI 2.0 integri da un capo all'altro.
- [ ] **5.6 Documentazione** · Claude · Guida all'installazione, impostazioni di rete consigliate, risoluzione dei problemi. Verifica: Mauro installa da zero seguendo solo la guida.
- [ ] **5.7 Prima release pubblica** · Mauro · Pubblicazione di "AudioGridder revamped" e annuncio dove gli utenti dell'originale la cercano: discussions del progetto originale, KVR.

Il supporto CLAP entrerà quando JUCE lo offrirà ufficialmente.
