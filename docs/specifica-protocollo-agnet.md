# AudioGridder – Specifica protocollo e modulo di rete Rust

Oct 17, 2026 · @Mauro Longone

## Scopo e vincoli

Questa specifica definisce il nuovo protocollo tra plugin e server di AudioGridder e il modulo Rust che lo implementa, sostituendo le patch di rete del fork JUCE. È la base da approvare prima di scrivere codice.

Decisioni già prese, dal [report di analisi](https://claude.ai/code/artifact/b0dcaca7-2a9b-48b4-858c-32a0cba746b8):

- **Piattaforme**: macOS 10.12+, Windows 10+, Linux. È il minimo comune tra JUCE 9 (macOS 10.11) e il compilatore Rust attuale (macOS 10.12).
- **Protocollo nuovo**, non compatibile con la 1.2.0: plugin e server si aggiornano insieme.
- **Audio su TCP**, con un livello di trasporto astratto che permetta di aggiungere UDP in seguito.
- **Latenza**: obiettivo sotto i 10 ms aggiunti su cavo diretto; funzionamento solido su Ethernet cablata con switch.
- **Rust prima di JUCE 9**: il modulo di rete nasce sull'attuale JUCE 7, poi rende possibile il passaggio a JUCE 9 ufficiale.

Fuori dallo scopo: hosting dei plugin, formati VST e AU, interfaccia grafica del plugin. Restano in C++ e JUCE.

## Architettura

Il modulo Rust vive su entrambi i lati: nel plugin e nel worker del server. Il server principale fa solo da accoglienza, poi ogni sessione ha il suo worker e le sue connessioni dirette.

&#91;embedded content: architettura · plugin, server, worker e i tre canali\]

La linea tratteggiata si usa una volta sola, all'apertura della sessione; tutto il traffico successivo corre sui tre canali verso il worker.

## Canali e trasporti

Ogni istanza del plugin apre una sessione con tre connessioni separate per priorità, invece delle quattro di oggi. Oggi il plugin usa cmdOut, cmdIn, audio e screen, tutte TCP verso una porta assegnata al volo dal server.

| Canale | Contenuto | Direzione | Trasporto | Priorità di rete |
| --- | --- | --- | --- | --- |
| Audio | Blocchi audio, eventi MIDI, posizione di trasporto, latenza riportata | Bidirezionale, sincrono a blocchi | TCP con TCP\_NODELAY, socket locale in modalità locale | DSCP EF (46), massima |
| Controllo | Comandi con risposta (carica plugin, parametri, preset) ed eventi spinti dal server | Bidirezionale, richieste con ID | TCP, socket locale in modalità locale | DSCP AF21, normale |
| UI | Fotogrammi della UI remota (WebP), mouse, tastiera, appunti | Fotogrammi dal server, input dal plugin | TCP con banda massima configurabile | DSCP CS1, bassa |
| Discovery | Annuncio dei server in rete | Multicast | mDNS, servizio `_agrid2._tcp` | n/d |

Scelte principali:

- **Un solo canale di controllo bidirezionale** al posto di cmdIn e cmdOut. Le risposte si abbinano alle richieste tramite un ID, gli eventi del server viaggiano sullo stesso canale.
- **Nome di servizio mDNS nuovo** (`_agrid2._tcp`): i server 1.2.0 e quelli nuovi non si vedono a vicenda, così nessuno tenta connessioni incompatibili.
- **Socket locali anche su Windows**: Windows 10 dalla versione 1803 supporta AF\_UNIX, quindi la modalità locale può usarli su tutte e tre le piattaforme.
- **Porte**: una porta di ascolto principale (55056 + ID server) più un intervallo configurabile per i worker, così chi ha un firewall sa esattamente cosa aprire.

## Formato dei messaggi

Tutti i byte sul filo vengono scritti e letti solo dal modulo Rust: il C++ riceve e consegna strutture tipizzate, mai buffer grezzi. È la regola che elimina alla radice i problemi delle struct C++ inviate così come sono in memoria.

**Frame.** Ogni messaggio, su qualsiasi canale, è un frame con intestazione fissa di 12 byte, little-endian (lo sono tutte le piattaforme supportate):

| Campo | Tipo | Significato |
| --- | --- | --- |
| magic | u16 | Costante 0x4147 ("AG"): rileva subito uno stream desincronizzato |
| kind | u8 | Tipo di frame (audio, richiesta, risposta, evento, ping, errore) |
| flags | u8 | Bit di servizio, per esempio payload compresso |
| length | u32 | Lunghezza del payload, massimo 16 MB |
| id | u32 | Numero di sequenza audio o ID della richiesta |

Nessun checksum applicativo: TCP e Ethernet lo fanno già, e il magic intercetta gli errori di framing.

**Codifica.** Il canale di controllo usa MessagePack, generato da strutture Rust con serde: compatto, veloce e leggibile in debug. Il canale audio usa un layout binario fisso, perché ogni microsecondo conta: intestazione del blocco, campioni in formato planare, poi gli eventi MIDI.

**Handshake.** La prima cosa inviata su ogni connessione:

1. Il plugin manda `Hello`: versione del protocollo, ID client, tipo di canale, token di sessione (vuoto alla prima connessione), capacità supportate, configurazione audio (canali, sample rate, dimensione del blocco, precisione).
2. Il server risponde `Welcome`: versione, ID di sessione, capacità del server, endpoint del worker.
3. Le connessioni successive della stessa sessione presentano il token: il server le aggancia alla sessione giusta.

**Versioning.** La versione ha due numeri: il maggiore deve coincidere, altrimenti la connessione viene rifiutata con un messaggio chiaro ("aggiorna il server alla 2.x"). Le aggiunte compatibili passano dalle capacità negoziate, senza cambiare versione.

## Catalogo messaggi

I 35 tipi di messaggio attuali (definiti in Common/Source/Message.hpp) si riducono a quattro gruppi e circa 20 messaggi, perché le coppie richiesta e risposta diventano un unico scambio con ID.

| Gruppo | Messaggi attuali | Nel nuovo protocollo | Canale |
| --- | --- | --- | --- |
| Sessione | Quit, Result, ServerError, Restart, CPULoad | `Close`, `Error` con codice e testo, `Restart`, evento `Metrics` periodico (CPU, latenza, buffer) | Controllo |
| Catena di plugin | PluginList, AddPlugin, AddPluginResult, DelPlugin, PluginStatus, SetMonoChannels, BypassPlugin, UnbypassPlugin, ExchangePlugins, RecentsList, Rescan | `ListPlugins`, `AddPlugin`, `RemovePlugin`, `MovePlugin`, `SetBypass`, `SetChannels`, `Rescan`; evento `ChainChanged` | Controllo |
| Parametri e stato | Parameters, ParameterValue, GetParameterValue, GetAllParameterValues, ParameterGesture, Presets, Preset, GetPluginSettings, SetPluginSettings, PluginSettings | `GetParameters`, `SetParameter`, `Gesture`, `ListPresets`, `LoadPreset`, `GetState`, `SetState`; evento `ParameterChanged` | Controllo |
| UI remota | EditPlugin, HidePlugin, GetScreenBounds, ScreenBounds, ScreenCapture, UpdateScreenCaptureArea, Mouse, Key, Clipboard | `OpenEditor`, `CloseEditor`, `Frame` (fotogramma o porzione), `Input` (mouse, tastiera), `Clipboard` | UI |

L'audio non compare qui: ha il suo frame binario dedicato, descritto nella sezione sul percorso audio.

Novità rispetto a oggi: gli **eventi spinti dal server** (`ParameterChanged`, `ChainChanged`, `Metrics`) tengono il plugin aggiornato senza che debba chiedere, e ogni richiesta ha un **timeout esplicito** con errore tipizzato invece del silenzio.

## Percorso audio e latenza

Il thread audio della DAW non tocca mai la rete: scrive il blocco in un buffer circolare senza lock e riceve il blocco elaborato D blocchi prima. La latenza aggiunta vale D blocchi, ed è riportata alla DAW per la compensazione automatica.

**Il frame audio** contiene: numero di sequenza (u64), posizione in campioni dell'host (u64), numero di campioni e di canali (u16), stato del trasporto (in riproduzione, BPM, posizione in battute, metrica), numero di eventi MIDI, poi i campioni in formato planare (float a 32 o 64 bit) e gli eventi MIDI con il loro offset.

**Budget di latenza** a 48 kHz, latenza aggiunta da AudioGridder esclusa quella della scheda audio:

| Blocco (campioni) | D = 1 | D = 2 | D = 3 |
| --- | --- | --- | --- |
| 64 | 1,3 ms | 2,7 ms | 4,0 ms |
| 128 | 2,7 ms | 5,3 ms | 8,0 ms |
| 256 | 5,3 ms | 10,7 ms | 16,0 ms |

Con il blocco da 128 l'obiettivo dei 10 ms regge fino a D = 3. Il vincolo fisico: andata in rete, elaborazione sul server e ritorno devono stare dentro D periodi di blocco, perché il blocco inviato al callback n torna in uscita al callback n + D.

**Jitter buffer adattivo.** Le DAW gestiscono male un cambio di latenza durante la riproduzione, quindi D si adatta solo a trasporto fermo:

- Il modulo misura di continuo il tempo di andata e ritorno di ogni blocco e ne tiene la distribuzione.
- **Modalità Auto**: a trasporto fermo sceglie il D minimo che copre il 99,9° percentile misurato, più un margine.
- **Modalità Fissa**: D lo sceglie l'utente, con il valore consigliato mostrato accanto. Pensata per il live.
- Un blocco in ritardo durante la riproduzione produce silenzio per quel blocco, incrementa il contatore dei dropout e segnala di alzare D alla prossima pausa.

**Regole realtime**, verificate da test automatici:

- Nel callback audio niente allocazioni, niente lock, niente chiamate di sistema. In test lo controlla il crate `assert_no_alloc`.
- I thread di rete audio girano con priorità realtime: MMCSS "Pro Audio" su Windows, time-constraint su macOS, SCHED\_FIFO su Linux. Se il sistema la nega, il modulo prosegue a priorità normale e lo segnala nelle metriche invece di fallire (è il bug Linux segnalato a ottobre 2026).

## Affidabilità

Una caduta di rete non deve mai bloccare la DAW né far perdere la catena di plugin: il plugin passa in modalità di emergenza, si riconnette da solo e riprende la sessione dove era.

**Heartbeat.** Sul canale di controllo un `Ping` ogni 500 ms; durante la riproduzione fa da heartbeat anche il flusso audio. Senza traffico per 2 secondi la connessione è considerata morta. Valori iniziali, da tarare nei test.

**Modalità di emergenza** mentre la connessione manca:

- Effetti: l'audio in ingresso passa invariato (bypass), così il mix resta udibile.
- Strumenti: breve fade-out verso il silenzio, per evitare il clic.
- Il plugin mostra lo stato "riconnessione in corso" con il motivo della caduta.

**Riconnessione con ripresa di sessione.**

1. Il plugin ritenta con attesa crescente: 100 ms, 200 ms, 500 ms, poi ogni secondo.
2. Si ripresenta con il token di sessione.
3. Il server tiene viva la sessione (worker, plugin caricati, loro stato) per 30 secondi dopo la caduta: se il token arriva in tempo, si riprende senza ricaricare nulla.
4. Se la sessione è scaduta, il plugin ricostruisce la catena dallo stato salvato nel progetto della DAW, come all'apertura.

**Errori tipizzati.** Ogni errore ha un codice e un testo leggibile, mostrato all'utente invece di un generico "No connection":

| Codice | Quando | Messaggio all'utente |
| --- | --- | --- |
| VERSION\_MISMATCH | Versione maggiore diversa | Quale lato aggiornare e a che versione |
| PORT\_IN\_USE | Il server non riesce ad aprire la porta | Quale porta è occupata e come cambiare l'ID del server |
| SESSION\_EXPIRED | Token arrivato dopo i 30 secondi | Catena ricaricata dallo stato del progetto |
| PLUGIN\_LOAD\_FAILED | Un plugin non si carica sul server | Nome del plugin e motivo riportato dal server |
| PLUGIN\_CRASHED | Il processo sandbox di un plugin è caduto | Nome del plugin; gli altri della catena continuano |
| TIMEOUT | Una richiesta non ha risposta entro il suo limite | Quale operazione e dopo quanto |

## Il modulo Rust

Il modulo è un workspace Cargo di quattro crate, compilato come libreria statica e collegato alla build CMake esistente tramite Corrosion. Nome di lavoro: `agnet`.

| Crate | Responsabilità | Fa I/O |
| --- | --- | --- |
| agnet-proto | Frame, codifica MessagePack, layout del frame audio, tipi dei messaggi | No: puro, testabile e sottoponibile a fuzzing |
| agnet-transport | Interfaccia `Transport` con implementazioni TCP e socket locale; UDP in futuro | Sì |
| agnet-session | Handshake, heartbeat, riconnessione, jitter buffer, metriche, discovery mDNS | Sì |
| agnet-ffi | Ponte verso il C++ con il crate `cxx`, unico punto di contatto | No |

**API vista dal C++, lato plugin** (nomi indicativi):

- `Client::connect(indirizzo, configurazione_audio)` restituisce un handle di sessione; non blocca.
- `push_block(campioni, midi, trasporto)` e `pull_block(destinazione)`: chiamate dal thread audio, realtime-safe, non bloccano mai. `pull_block` restituisce anche lo stato del blocco (pronto, in ritardo, disconnesso).
- `request(richiesta)` restituisce un ID; risposte ed eventi arrivano con `poll_events()`, chiamata dal thread dei messaggi di JUCE.
- `send_input(evento)` e `poll_frame()` per la UI remota.

Il lato server ha l'API speculare: `Server::listen`, accettazione delle sessioni, `recv_block` e `send_block` nel worker.

**Threading.** Per ogni sessione il modulo crea tre thread propri: rete audio (priorità realtime), controllo e UI. Nessun runtime async: socket bloccanti con timeout, comportamento più prevedibile per la latenza. Il thread audio della DAW comunica solo attraverso buffer circolari a produttore e consumatore singolo, preallocati (crate `rtrb`).

**Sicurezza al confine.** Ogni funzione esposta al C++ intercetta i panic Rust e li trasforma in un evento di errore: un bug nel modulo non deve mai far cadere la DAW.

**Dipendenze previste**, volutamente poche: `serde` e `rmp-serde` (codifica), `socket2` (TCP\_NODELAY, DSCP), `rtrb` (buffer circolari), `mdns-sd` (discovery), `cxx` (ponte), `thiserror` (errori). Versione minima di Rust fissata nel repository.

## Test e criteri di accettazione

Il modulo è accettato quando supera tutti gli scenari simulati qui sotto e poi la prova reale in Cubase, su cavo diretto e su switch. Gli scenari girano in automatico su Linux con `tc netem`, che aggiunge ritardo, jitter e perdite controllate; riferimento a 48 kHz con blocco da 128 campioni.

| Scenario | Rete simulata | Criterio di accettazione |
| --- | --- | --- |
| Cavo diretto | Nessun ritardo aggiunto | D = 1 stabile, zero dropout in 30 minuti |
| LAN con switch, tranquilla | 0,1 ms ± 0,05 ms | D ≤ 2, zero dropout in 30 minuti |
| LAN trafficata | 0,5 ms ± 0,5 ms, 0,01% di perdite, trasferimento di file in parallelo | D ≤ 3 (sotto i 10 ms), dropout solo in corrispondenza di pacchetti persi |
| Rete pessima | 2 ms ± 2 ms, 0,5% di perdite | Nessun blocco della DAW, metriche che segnalano il problema, D consigliato coerente |
| Cavo staccato per 5 secondi | Interruzione totale | Modalità di emergenza, ripresa della sessione senza ricaricare i plugin |

Il limite onesto del TCP: un pacchetto perso viene ritrasmesso, nel caso peggiore dopo il timeout minimo del sistema (200 ms su Linux), e quel blocco va in dropout. Su cavo e LAN cablate le perdite sono rarissime; sulle reti con perdite frequenti servirà UDP (vedi le decisioni in fondo).

Test di base, eseguiti a ogni modifica:

- Codifica e decodifica di ogni messaggio avanti e indietro, più fuzzing del decoder con `cargo-fuzz`: dati malformati non devono mai causare crash.
- Verifica assenza di allocazioni nel percorso realtime con `assert_no_alloc`.
- Build su macOS, Windows e Linux con GitHub Actions.

## Decisioni del 17 ottobre 2026

- **Licenza**: AGPLv3, tramite la licenza open source di JUCE. Il progetto resta open source.
- **Nome provvisorio**: "AudioGridder revamped", da rivedere più avanti.
- **Porte**: intervallo configurabile per i worker ora; porta unica come miglioramento futuro.
- **UDP**: dopo che il TCP ha superato i test, come fase dedicata, con ridondanza dei pacchetti audio.
- **Sicurezza**: nessuna cifratura sul traffico.
- **Strumenti in emergenza**: breve fade-out verso il silenzio, per evitare il clic.

Ancora da decidere:

- [ ] Serve un codice di abbinamento tra plugin e server, così che non chiunque nella rete possa connettersi?
