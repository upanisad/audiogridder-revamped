# Strumento di prova della rete (agnet-tool)

Piccolo programma che simula una DAW e un server di plugin, scambia audio sintetico tra due macchine e misura **latenza aggiunta, dropout e tempi di rete**. Serve a provare il cavo diretto e lo switch prima di integrare il nuovo modulo di rete in AudioGridder.

Non è AudioGridder: non carica plugin e non tocca Cubase. Simula solo il percorso dell'audio.

## 1. Scaricare i programmi

1. Apri https://github.com/upanisad/audiogridder-revamped/actions/workflows/rust.yml
2. Clicca sul run più recente con la spunta verde.
3. In fondo alla pagina, nella sezione **Artifacts**, scarica:
   - `agnet-tool-windows-x64` per il PC Windows
   - `agnet-tool-linux-x86_64` per il muletto (se è un PC o un server normale a 64 bit; se è un Raspberry o simile dimmelo)
4. Sono file `.zip`: decomprimi, dentro c'è un solo file.

## 2. Sul muletto Linux (server)

Copia il file sul muletto (per esempio con `scp`), poi:

```bash
chmod +x agnet-tool
./agnet-tool server
```

Deve scrivere `in ascolto su [::]:55100` e `Server pronto`. Lascialo acceso.

- Se hai un firewall: `sudo ufw allow 55100/tcp` (o l'equivalente della tua distribuzione).
- Per sapere l'indirizzo IP del muletto: `ip -4 addr`.
- Se la porta è occupata scegline un'altra: `./agnet-tool server --port 55200` (poi la stessa sul client).

## 3. Sul PC Windows (client)

Apri **PowerShell** nella cartella dove hai messo il file. La prima volta Windows può mostrare «Windows ha protetto il PC» (SmartScreen), perché il programma non è firmato: clicca **Ulteriori informazioni** e poi **Esegui comunque**.

```powershell
.\agnet-tool.exe client --server 192.168.1.50
```

(sostituisci con l'IP del muletto). Dopo una misura iniziale di 3 secondi parte la riproduzione e ogni 5 secondi compare una riga. In fondo c'è il riepilogo.

## 4. Le prove da fare

Prima di ogni prova: chiudi Cubase e gli altri programmi pesanti sul PC, piano di alimentazione su **Prestazioni elevate**, Wi-Fi spento.

| # | Collegamento | Comando | Cosa guardare |
|---|---|---|---|
| 1 | Cavo diretto | `.\agnet-tool.exe client --server IP --duration 60` | D, dropout |
| 2 | Cavo diretto, blocco piccolo | `... --block 64 --duration 60` | D, dropout |
| 3 | Switch | `... --duration 60` | D, dropout |
| 4 | Switch, blocco grande | `... --block 256 --duration 60` | D, dropout |
| 5 | La più importante: lunga | `... --duration 1800 --interval 30 --csv prova30min.csv` | dropout su 30 minuti |
| 6 | Strumento (cosa succede se cade) | `... --kind instrument --duration 60` | come la 1 |

Per il cavo diretto servono due indirizzi fissi, per esempio 192.168.10.1 sul PC e 192.168.10.2 sul muletto, con maschera 255.255.255.0.

Il blocco (`--block`) corrisponde al buffer della scheda audio in Cubase: provalo con i valori che usi davvero.

## 5. Come leggere il risultato

- **Latenza aggiunta**: quanto AudioGridder aggiunge al percorso. L'obiettivo è **sotto i 10 ms** su cavo diretto. `D` è il numero di blocchi di ritardo scelto dal programma in base alla rete.
- **Dropout**: i blocchi arrivati troppo tardi (in cuffia: un buchino o un clic). Obiettivo: **0** sul cavo diretto e sullo switch tranquillo.
- **Andata e ritorno**: tempo che l'audio impiega ad andare al muletto e tornare. La mediana è il caso normale; *p99* e *massimo* mostrano i picchi.
- **Disturbi locali**: se il PC stesso si è svegliato in ritardo, i dropout possono essere colpa sua e non della rete. Il programma lo dice nell'ultima riga.

Per forzare un ritardo: `--delay 2` (da 1 a 8 blocchi). Con `--delay auto` (predefinito) decide il programma.

## 6. Cosa mandarmi

Copia e incolla tutto il testo che il programma stampa (anche con `--csv` il file, se hai fatto la prova da 30 minuti). Dimmi per ogni prova: cavo o switch, dimensione del blocco, e che macchine sono. Con quei numeri compilo la tabella delle misure.

## 7. Se qualcosa non va

- **«Prova non riuscita»**: il server non è raggiungibile. Controlla che sia avviato, l'indirizzo IP, il firewall del muletto e che i due siano nella stessa rete (`ping IP`).
- **Molti dropout con «il PC è arrivato in ritardo»**: chiudi altri programmi, spegni il Wi-Fi, riprova.
- **Sul muletto compare «porta già occupata»**: c'è già un server avviato oppure un altro programma usa la 55100; usa `--port`.
