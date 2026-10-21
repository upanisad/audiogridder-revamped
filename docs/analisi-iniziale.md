# AudioGridder – Analisi iniziale per il fork

Oct 17, 2026 · @Mauro Longone

## In breve

AudioGridder è di fatto fermo da marzo 2024 e oggi si rompe sui sistemi recenti (macOS 15 e 26, Logic 11 e 12): un fork ha senso, e il lavoro è chiaro.

- **Base tecnica vecchia**: gira su un fork personale di JUCE 7.0.5 (gennaio 2023). Oggi siamo a JUCE 9.0.4, uscito il 12 ottobre 2026.
- **Le patch dell'autore non sono mai entrate in JUCE ufficiale**: 19 commit su 14 file, quasi tutti sul layer di rete (socket Unix, IPv6) e sui processi figli della sandbox. Vanno riportate a mano.
- **Scoperta chiave**: proprio quel layer di rete patchato è il candidato naturale per Rust. Spostandolo in un modulo Rust, AudioGridder potrebbe tornare a usare JUCE ufficiale senza fork.
- **JUCE 9 non ha ancora il supporto CLAP**, nonostante fosse annunciato. Alza però i requisiti minimi a macOS 10.11 e Windows 10: addio Windows 7 e vecchi Mac.
- **Licenza**: JUCE 9 è AGPLv3 oppure commerciale. Va deciso subito come distribuire il fork.

## Stato del progetto

L'ultimo commit su master è del 29 marzo 2024: un refactoring della finestra impostazioni del server, fatto da un contributore esterno. L'ultima release resta la 1.2.0 del marzo 2023.

| Voce | Valore |
| --- | --- |
| Codice proprio | circa 55.700 righe C++ (Common, Server, Plugin, PluginTray, Tests) |
| Build | CMake + vcpkg + build.py |
| Dipendenze | Boost 1.83, libwebp 1.3.2, FFmpeg 6.0 |
| JUCE | fork apohl79/JUCE, base 7.0.5 + 19 commit propri |
| Licenza | MIT |
| Comunità | 873 stelle, 95 fork, 12 issue aperte, 29 pagine di discussions |

L'autore chiede di segnalare i problemi nelle discussions, non nelle issue. Le richieste più recenti, fino a ottobre 2026, restano senza risposta, e una discussion intitolata "Is this project dead?" ha 24 commenti. Non ho potuto verificare se esistano già fork attivi: GitHub blocca la lettura automatica della pagina dei fork, da controllare a mano.

## Le patch al JUCE dell'autore

Nessuna delle patch esiste in JUCE 9.0.4: le ho cercate una per una nel sorgente ufficiale. Il diff totale è di 817 righe aggiunte e 354 tolte su 14 file, e si raggruppa in quattro blocchi.

| Blocco | Cosa fa | File JUCE toccati | Usato da AudioGridder in |
| --- | --- | --- | --- |
| Socket di rete | Socket Unix domain (bindToPath, connect su file, createListener su path), listener IPv6 (createListener6, bindToPort6), rimozione di inet\_ntoa | juce\_Socket.cpp/.h (+361 righe) | Server.cpp, ProcessorClient.cpp, Client.cpp del plugin |
| Connessioni IPC | connectToSocket e beginWaitingForSocket su socket Unix | juce\_InterprocessConnection, juce\_InterprocessConnectionServer | Comunicazione tra processi del server |
| Processi figli (sandbox) | Argomenti custom al lancio del worker, timeout di ping separato, ping solo a connessione avvenuta, niente processi zombie su POSIX | juce\_ConnectedChildProcess (riscritto per circa 600 righe), juce\_posix\_SharedCode.h | Sandbox.hpp, Server.cpp (launchWorkerProcess) |
| AudioUnit e build | Canali AU impostabili a mano (getAUChannelInfo), build per macOS precedenti alla 10.9 | juce\_AU\_Shared.h, juce\_AudioProcessor.h, juce\_TargetPlatform.h | Plugin AU |

I primi due blocchi sono il trasporto di rete vero e proprio: è lì che un modulo Rust può sostituire le patch invece di riportarle. Il terzo si può riscrivere come launcher dei processi dentro AudioGridder stesso. Il quarto è piccolo: JUCE 9 calcola già i canali AU da sé (helper interno, non sovrascrivibile), e il supporto a macOS sotto la 10.9 decade comunque.

## JUCE 9: cosa porta e cosa costa

JUCE 9.0.0 è uscito il 21 luglio 2026 e la 9.0.4 il 12 ottobre 2026. Il 13 ottobre sono uscite anche patch di sicurezza per i rami vecchi (8.0.16-security e 7.0.13-security). Dal changelog ufficiale, le voci che contano per AudioGridder:

| Versione | Novità utile per AudioGridder |
| --- | --- |
| 9.0.4 | Fix su disconnessione dispositivi MIDI e MessageManagerLock, fix tray e finestre su Linux, protezione da stringhe malformate |
| 9.0.3 | Fix ALSA su Linux, supporto WebP nativo (oggi AudioGridder usa libwebp esterna per lo streaming UI) |
| 9.0.2 | Fix all'hosting VST3, nuova gestione CoreAudio |
| 9.0.1 | Pacchetto npm TypeScript per le UI in WebView |
| 9.0.0 | Nuova implementazione CoreAudio su macOS, build CMake migliore per ambienti headless |

I vincoli:

- **Requisiti minimi**: deployment da macOS 10.11 e Windows 10 1607, compilazione in C++17. AudioGridder oggi dichiara macOS 10.7 e Windows 7.
- **Niente CLAP**: nel sorgente 9.0.4 non c'è traccia di CLAP, né per creare plugin né per ospitarli.
- **VST2 resta**: l'hosting VST2 c'è ancora, ma serve l'SDK VST2 di Steinberg, non più distribuito.
- **Licenza**: JUCE 9 è in doppia licenza, AGPLv3 o commerciale. Distribuendo il fork gratis e open source, la via AGPLv3 è la più semplice: il codice MIT di AudioGridder è compatibile, ma i binari risultanti ricadono sotto AGPLv3. Va confermato con calma prima di pubblicare release.

## Mappa dei dolori

Il dolore principale è la compatibilità con i sistemi nuovi, non i bug di logica: quasi metà delle segnalazioni recenti viene da macOS 15/26, Logic e Linux. Ho letto le 12 issue aperte e la prima pagina delle discussions (25 su circa 29 pagine); l'API di GitHub non era accessibile da qui, quindi i dettagli dei thread restano da approfondire.

| Categoria | Segnalazioni | Fonte | Causa probabile |
| --- | --- | --- | --- |
| macOS recenti e Logic | Plugin non funzionanti su macOS 26 Tahoe (#1475, mar 2026); macOS 15 (giu 2026, set 2024); strumento assente in Logic 11 (27 commenti); MacBook M4 Max | Issue e discussions | JUCE 7.0.5 precede macOS 15 e 26: aggiornare JUCE è la prima cura |
| Connessione e rete | "No Connection to the Server" (#1458); porta 55056 già occupata (#1455); timeout lato server (ago 2026); richiesta di auto-rilevare la connettività (#1387) | Issue e discussions | Layer socket patchato, gestione errori e riconnessione |
| Linux | Server bloccato su "Loading plugin layouts" (feb 2026); crash del server sperimentale (set 2025); audio fallisce se il thread realtime viene negato (ott 2026); Ubuntu 24.04, Fedora 39 | Discussions | Server Linux mai completato, priorità realtime non gestita |
| UI dei plugin | Finestre aperte sei volte (#1460); "GUI nightmare" (#1456); GUI bloccata con ShaperBox 3 (#1403); GUI SoundToys rotta dalla 1.2.0b7 | Issue e discussions | Cattura e streaming della UI, gestione finestre |
| Audio e canali | Strumento VST3 senza audio (#1415); non tutti i canali attivi (34 commenti); rumore all'avvio | Issue e discussions | Layout dei bus, inizializzazione dei buffer |
| Sessioni e server multipli | Istanze locali scambiate al ricaricamento con più server (#1409); plugin disabilitati dopo lo scan (set 2026) | Issue e discussions | Identificazione server e cache dei plugin |
| Windows 11 | Due segnalazioni di errori sul server (#1435, #1436) | Issue | Da capire: dettagli scarni |

Le categorie rete, Linux e audio sono quelle dove Rust aiuta davvero. macOS e Windows si risolvono soprattutto con l'aggiornamento di JUCE.

## Roadmap rivista

La roadmap cambia in un punto: Rust entra per sostituire le patch di rete, così AudioGridder torna su JUCE ufficiale senza fork da mantenere. Ogni fase si chiude solo se il cancello è superato nei tuoi test reali in Cubase.

&#91;embedded content: roadmap del fork · 5 fasi, 4 cancelli\]

La fase 2 è il cambio di rotta rispetto al piano iniziale: Rust non si aggiunge sopra JUCE, ne prende il posto dove il fork era patchato, e solo dopo si passa a JUCE 9 ufficiale.

Decisioni da prendere prima di partire:

- [ ] Piattaforme minime: deciso il 17 ottobre 2026, macOS 10.11+ e Windows 10+, Windows 7 e vecchi Mac abbandonati
- [ ] Licenza di distribuzione del fork: deciso il 17 ottobre 2026, AGPLv3, il progetto resta open source. Nome provvisorio: "AudioGridder revamped"
- [ ] Su quale macchina gira il server di test: Mac, Windows o Linux?
- [ ] Rotta iniziale: deciso il 17 ottobre 2026, subito il modulo Rust al posto delle patch, poi JUCE 9 ufficiale
- [ ] Controllare a mano la pagina dei fork su GitHub, per non rifare lavoro già fatto da altri.

Protocollo plugin-server: deciso il 17 ottobre 2026, protocollo nuovo e non compatibile con la 1.2.0; plugin e server si aggiornano insieme. Oggi il protocollo (versione 13) invia struct C++ grezze sul socket, con 35 tipi di messaggio e payload misti binari, JSON e MsgPack.

Trasporto: deciso il 17 ottobre 2026, TCP per l'audio con trasporto astratto in Rust (TCP, socket Unix, UDP aggiungibile dopo), jitter buffer adattivo. Obiettivo: sotto i 10 ms su cavo diretto e ottimo funzionamento su Ethernet cablata con switch, in vista della pubblicazione.

## Fonti

- [apohl79/audiogridder su GitHub](https://github.com/apohl79/audiogridder): sorgente clonato e analizzato, storico dei commit
- [Fork JUCE dell'autore](https://github.com/apohl79/JUCE) confrontato con [JUCE ufficiale](https://github.com/juce-framework/JUCE), tag 9.0.4: diff, CHANGE\_LIST.md, README.md, LICENSE.md
- [Issue aperte](https://github.com/apohl79/audiogridder/issues) e [discussions](https://github.com/apohl79/audiogridder/discussions), prima pagina
- [Issue #1475, macOS 26 Tahoe](https://github.com/apohl79/audiogridder/issues/1475)
- [Sito AudioGridder](https://audiogridder.com/)
