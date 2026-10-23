# HANDOFF — AudioGridder revamped

Documento di passaggio di consegne verso Claude Code (locale, sul PC Windows di Mauro).
Data: 2026-10-21. Stato: fasi 1.1, 1.2, 1.3 completate; baseline compilata e funzionante anche su Windows.

---

## 1. Chi e come lavorare

- **Utente:** Mauro Longone (Italia). Esperto PHP/JavaScript, sistemista Linux, SQL/MongoDB, WordPress/WooCommerce. Musicista (Cubase, sintetizzatori, VST). **Non ha mai scritto C/C++**: lui progetta e decide l'architettura, **il codice lo scrive Claude**.
- **Lingua e tono:** italiano, amichevole, colloquiale, simpatico. Spiegazioni passo passo quando si tratta di strumenti Windows/C++ (non li conosce). Risposte concise, niente giri di parole.
- **Ambiente di Mauro:** Windows con Visual Studio 2022 Community (installato anche la 2026, **non usarla**), VS Code, Git. Server di prova ("muletto"): macchina Linux headless collegata al PC con cavo diretto.
- **Convenzioni git:** i commit terminano con le righe
  ```
  Co-Authored-By: Claude <noreply@anthropic.com>
  Claude-Session: <URL della sessione, se disponibile>
  ```
  Le descrizioni delle PR terminano con "Generated with Claude Code" + URL di sessione, se presente.
- Non avviare passi successivi senza il via libera di Mauro.

## 2. Obiettivo del progetto

Fork e modernizzazione di **AudioGridder** (bridge open source C++/JUCE che sposta l'elaborazione dei plugin audio su macchine remote), nome provvisorio **"AudioGridder revamped"**.

Obiettivi: **più stabilità** e **miglioramenti**, con approccio **ibrido**:
- si tiene il codice C++/JUCE per plugin, server, GUI, hosting dei plugin;
- il livello di rete viene **riscritto in Rust** (modulo `agnet`), sostituendo la rete della fork JUCE patchata dall'autore;
- **protocollo nuovo e pulito**, **non compatibile con la 1.2.0**;
- licenza **AGPLv3**, resta open source;
- nessuna cifratura (deciso).

**Vincolo critico: la latenza.** È per musica dal vivo. Oltre 40 ms è fastidioso. Obiettivo: **meno di 10 ms aggiunti** sul cavo diretto PC↔muletto, e comportamento buono su Ethernet cablata convenzionale con switch (per la pubblicazione).

## 3. Repository e stato

- Repo: https://github.com/upanisad/audiogridder-revamped (fork di apohl79/audiogridder, versione 1.2.0, ultimo commit originale 2024-03-29).
- Branch di lavoro: **`ci/build-baseline`**. Contiene la CI e la correzione per MSVC (`#include <chrono>` in `Common/Source/Utils.hpp`).
- **PR #1** (https://github.com/upanisad/audiogridder-revamped/pull/1, base `master`): contiene tutto il lavoro dei passi 1.2 e 1.3. **Da rivedere e fare merge da parte di Mauro.**
- CI: `.github/workflows/build.yml`. Matrice Linux x86_64 (ubuntu-24.04), macOS arm64 (macos-14, Xcode 15.4), Windows x64 (windows-2022). **Tutte e tre verdi** (ultimo run 38992049754). Produce artifact con i binari.

### Esito dei passi
| Passo | Stato |
|---|---|
| 1.1 Fork su GitHub | Fatto |
| 1.2 Build su Linux | Fatto e verificato (plugin + server) |
| 1.3 Build automatica su 3 piattaforme | Fatto, CI verde |
| Build Windows locale | Fatta da Mauro con VS 2022, **funziona** (plugin caricato in Cubase, server avviato) |
| 1.4 Misure di riferimento con 1.2.0 | **Da fare** (Mauro, con procedura scritta da Claude) |
| 1.5 Gate: la build del fork suona come la 1.2.0 | Da fare |

Mauro sta usando la build del fork come sostituto della versione ufficiale. Il server ha ripreso impostazioni ed elenco plugin della vecchia installazione (cartella `%APPDATA%\AudioGridder`). Sconsigliato mischiare plugin e server di versioni diverse.

## 4. Come compilare su Windows (verificato)

Terminale: "Developer PowerShell for VS 2022", dentro il repo (`C:\Users\upani\GitHub\audiogridder-revamped`).

```powershell
git checkout ci/build-baseline
git submodule update --init --recursive

# Dipendenze precompilate dell'autore (solo cartella Windows, commit fissato)
git clone --filter=blob:none --no-checkout https://github.com/apohl79/audiogridder-deps ag-deps
cd ag-deps
git sparse-checkout set windows-x86_64
git checkout 6a7a41c06b9ca9d4a260febe79ee1f6e96618e0a
cd ..

cmake -S . -B build -G "Visual Studio 17 2022" -A x64 `
  -DAG_DEPS_ROOT="$PWD/ag-deps/windows-x86_64" `
  -DAG_ENABLE_CODE_SIGNING=OFF -DAG_ENABLE_DEBUG_COPY_STEP=OFF -DAG_WITH_TESTS=OFF

cmake --build build --config RelWithDebInfo --parallel 4
```

Risultati utili: `build\bin\AudioGridderServer.exe`, `build\bin\AudioGridderPluginTray.exe`, `build\lib\VST3\AudioGridder{,Inst,Midi}.vst3`. Le cartelle `build\debug\...` sono copie intermedie.
Installazione plugin: copiare i `.vst3` in `C:\Program Files\Common Files\VST3\` (PowerShell da amministratore).

Linux: build con `-G Ninja` e le dipendenze `linux-x86_64`; pacchetti elencati nel workflow (inclusi `libxcb1-dev libxcb-shm0-dev libxcb-xfixes0-dev libxcb-shape0-dev`).
macOS: Xcode 15.4 (JUCE 7.0.5 non compila con l'SDK di macOS 15), `-DCMAKE_OSX_ARCHITECTURES=arm64 -DAG_MACOS_TARGET=11.1`. Binari non firmati: serve `xattr -dr com.apple.quarantine`.

## 5. Problemi noti

- **Test originali non eseguibili** (`AG_WITH_TESTS=OFF`): file ObjC++ su Linux, refuso `findPluginDescritpion` in `Tests/Source/Server/MultiMonoTest.hpp`, mancano header VST2 e plugin di terze parti. Non corretti; i test nuovi arrivano nella fase 2.
- **Server senza IPv6:** su macchine/container senza IPv6 fallisce con "failed to create master listener" perché chiama solo `createListener6`. Da risolvere nel nuovo trasporto Rust, non patchare il vecchio.
- JUCE 7.0.5 con la fork dell'autore (base 7.0.5 + 19 commit: socket Unix, listener IPv6, `ConnectedChildProcess` riscritto con argomenti e timeout ping, `getAUChannelInfo`, macOS ≥ 10.9). **Nessuno di questi commit è presente in JUCE 9.0.4.**
- Rete di rilievo: i binari non sono firmati (SmartScreen/Gatekeeper avvisano).

## 6. Decisioni già prese

- **Piattaforme minime:** macOS 10.12+ (minimo di Rust), Windows 10+, Linux.
- **Licenza:** AGPLv3. **Cifratura:** nessuna.
- **Sicurezza:** *aperta* la proposta di un **PIN di abbinamento opzionale** (spento di default, 6 cifre, verificato una volta nell'handshake). Mauro non ha ancora risposto.
- **Emergenza (rete caduta):** effetti in bypass; strumenti con breve fade-out.
- **Sessione:** conservata 30 s per la riconnessione.
- **Heartbeat:** 500 ms, considerato morto dopo 2 s.
- **Nome:** "AudioGridder revamped" per ora; si decide più avanti.

## 7. Architettura di rete (specifica sintetica)

Workspace Rust **`agnet`** con crate: `agnet-proto`, `agnet-transport`, `agnet-session`, `agnet-ffi`. Compilato come libreria statica e integrato in CMake con **Corrosion**, interfaccia C++ con **`cxx`**. Niente runtime asincrono. Panic catturati al confine FFI.

- **Trasporti:** TCP (`TCP_NODELAY`; DSCP EF per audio, AF21 controllo, CS1 UI), socket Unix locali, UDP più avanti con ridondanza dei pacchetti.
- **Intestazione frame, 12 byte, little-endian:** `magic 0x4147` (u16), `kind` (u8), `flags` (u8), `length` (u32), `id` (u32).
- **Controllo:** MessagePack (`serde` / `rmp-serde`). **Audio:** frame binario fisso, planare.
- **Handshake:** Hello/Welcome con token di sessione.
- **Jitter buffer adattivo:** modalità Auto/Fisso; cambia solo a trasporto fermo; latenza D blocchi.
- **Tempo reale:** code lock-free (`rtrb`), `assert_no_alloc`, priorità thread realtime con ripiego, errori tipizzati (`thiserror`).
- **Rete locale:** servizio mDNS `_agrid2._tcp` (`mdns-sd`), `socket2`, intervallo di porte per i worker.
- **Test:** `tc netem` per simulare rete difettosa, `cargo-fuzz`.

Protocollo attuale (1.2.0) per confronto: versione 13, struct C++ grezze su socket, `Header{type,size}`, 35 tipi di messaggio, payload misti binario/JSON/MsgPack, 4 connessioni TCP (comandi in uscita e in entrata, audio, schermo), schermo con FFmpeg/WebP. File chiave: `Common/Source/Message.hpp`, `Plugin/Source/Client.cpp`, `Server/Source/ScreenRecorder.cpp`.

## 8. Piano di lavoro (5 fasi, 31 step)

1. **Fase 1 – Baseline:** fork, build Linux, CI, misure di riferimento (1.4), gate di equivalenza con la 1.2.0 (1.5).
2. **Fase 2 – Rust `agnet`:** da 2.1 (workspace Cargo) a 2.11 (gate): protocollo, trasporto, sessione, jitter buffer, FFI, integrazione C++, test di rete.
3. **Fase 3 – JUCE 9:** aggiornamento da 7.0.5 a 9.0.4 (AGPLv3/commerciale, minimi macOS 10.11 / Windows 10 1607 / C++17; niente CLAP; VST2 richiede ancora l'SDK Steinberg; VST3 SDK 3.8 MIT). Va rifatto o sostituito ciò che la fork dell'autore aggiungeva.
4. **Fase 4 – Linux e sandbox:** server Linux headless stabile, isolamento dei plugin.
5. **Fase 5 – Miglioramenti e rilascio.**

Documenti completi (in italiano, su claude.ai, **riassunto di riferimento**):
- Report di analisi: https://claude.ai/code/artifact/b0dcaca7-2a9b-48b4-858c-32a0cba746b8
- Specifica del protocollo e della rete: https://claude.ai/code/artifact/e6fa75cc-e754-4f9a-ada4-1b3d5825993c
- Piano di lavoro: https://claude.ai/code/artifact/13e28161-8b49-401b-b894-f1c28c63148c
(I link richiedono l'accesso all'account di Mauro. Le caselle da spuntare del piano le gestisce lui.)

## 9. Prossimi passi

1. **Mauro**: rivedere e fare merge della PR #1; spuntare 1.2 e 1.3 nel piano.
2. **Step 1.4** (procedura da scrivere se Mauro dice sì): misure in Cubase con la 1.2.0, latenza, dropout, CPU, sia su cavo diretto sia attraverso uno switch.
3. **Step 1.5**: confermare nella stessa sessione che la build del fork suona come la 1.2.0.
4. **Decisione aperta:** PIN di abbinamento opzionale (sì/no).
5. Poi **Fase 2.1**: creare il workspace Cargo `agnet`.

## 10. Avvertenze per chi prosegue

- Non toccare il protocollo vecchio: sarà sostituito, non migrato. Mantenere la baseline compilabile finché la fase 2 non è pronta.
- Prima di ogni modifica sensibile alla latenza, avere un numero di riferimento misurato (step 1.4).
- Non usare Visual Studio 2026 né JUCE 9 prima della fase 3: la baseline usa JUCE 7.0.5 e le librerie precompilate dell'autore.
- Non dare per scontato nulla su Windows: Mauro non conosce la toolchain, spiegare ogni comando e indicare cosa controllare dopo.
