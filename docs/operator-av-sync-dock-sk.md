# Postup: kontrola A/V synchronizácie (dock "Audio Video Sync") — pre obsluhu

Jednostránkový postup pre obsluhu prenosu (nie pre vývojára). Slúži na overenie, či je obraz
a zvuk vo vysielaní časovo zarovnaný (žiadne "posunuté pery"), a na dolaďovanie, ak nie je.

## Čo to je

V OBS na počítači **stream** (10.77.9.204) je panel ("dock") s názvom **"Audio Video Sync"**.
Keď sa spustí, počúva PRESNE ten istý zvuk a obraz, ktorý ide do živého vysielania (program +
finálny zvukový mix) — nie žiadnu odbočku ani skúšobný signál naviac. Ak dock ukáže hodnotu
"Latency" blízku 0, obraz a zvuk sú zarovnané. Ak nie, dolaďuje sa to jedným nastavením
("Latency (ms)" na zdroji "NDI 2ME PGM"), popísaným nižšie.

**Ak dock nič neukazuje (samé pomlčky `-`):** buď testovací tón nebeží, alebo zvuková vetva
(mbc/Ableton) je momentálne stlmená/vypnutá — pozri krok 2 a 3.

## Kedy to robiť

- Pred väčšou udalosťou, ak si nie si istý, že je zvuk a obraz zarovnaný.
- Po akejkoľvek zmene v zvukovej ceste (nový mikrofón, zmena v Ableton, reštart OBS na `strih`
  alebo `stream`, výmena kamery).
- Orientačne raz za čas, aj keď sa nič nezmenilo — zarovnanie sa môže časom nepatrne posunúť.

## Krok za krokom

### 1. Over, že beží mbc (Master Broadcast Console)

mbc je počítač (10.77.9.232) s Ableton, ktorý robí finálne "mastrovanie" zvuku pre vysielanie.
Dock číta PRESNE ten zvuk, čo z neho vyjde — ak mbc/Ableton nebeží, dock nemá čo počuť.
Over, že je počítač zapnutý a Ableton beží.

### 2. V Ableton (na mbc) dočasne ODMUTUJ mikrofónový kanál pre meranie

Meranie potrebuje počuť skúšobný tón, ktorý sa posiela cez rovnaký mikrofón/kanál, čo bežne
ide do vysielania. Preto:

- **Pred meraním:** v Ableton na mbc nájdi kanál, ktorým ide tento mikrofón, a **ODMUTUJ ho**
  (zapni zvuk).
- Toto je JEDINÝ moment, kedy má byť tento kanál odmutovaný — pozri krok 6, kde sa vracia späť.

### 3. Spusti testovací signál na kamere cam2

Na počítači, odkiaľ sa ovláda rig (dev1), spusti testovací režim:

```
scripts/rig-mode.sh test
```

Toto rozsvieti na monitore pri cam2 testovací QR kód a pustí krátky pípavý tón z reproduktora
pri tom istom monitore — presne to, čo dock potrebuje vidieť/počuť. Skript sám potvrdí, že tón
skutočne hrá ("QPSK audio marker RUNNING").

### 4. V OBS na počítači "stream" otvor dock "Audio Video Sync"

Ak dock nie je vidno v OBS: hore v menu **View → Docks → Audio Video Sync** (zaškrtni/zapni).
Ak je už niekde pripnutý (napr. v bočnom paneli), len ho nájdi a klikni naň.

### 5. Klikni "Start" a sleduj hodnoty

V docku klikni tlačidlo **Start**. Do pár sekúnd by sa mali začať napĺňať polia:

| Pole v docku | Čo znamená |
|---|---|
| **Latency** | O koľko je zvuk a obraz mimo seba (v ms). Cieľ: blízko **0**. |
| (pod Latency) | "Audio lagged" = zvuk ide neskôr; "Audio early" = zvuk predbieha obraz. |
| **Index / Audio Index / Video Index** | Interné čísla, podľa ktorých dock páruje obraz a zvuk — netreba im rozumieť, len že sa MENIA (nie samé pomlčky). |
| **Audio Frequency** | Nameraná frekvencia testovacieho tónu — potvrdenie, že dock naozaj počuje ten správny tón. |

**Ak po ~10-15 sekundách zostávajú samé pomlčky `-`:** dock nič nepočuje/nevidí. Over znova
krok 1-3 (mbc zapnuté? kanál odmutovaný? testovací tón naozaj beží?) — pozri aj sekciu
"Keď to nefunguje" nižšie.

### 6. Ak "Latency" nie je blízko 0 — dolaď

1. V OBS na počítači "stream" nájdi v paneli **Sources** (Zdroje) položku **"NDI 2ME PGM"**.
2. Klikni na ňu pravým tlačidlom → **Properties** (Vlastnosti) — alebo dvojklik.
3. Nájdi pole **"Latency (ms)"** a uprav hodnotu (pár ms hore/dole).
4. Sleduj dock naživo — hodnota "Latency" v docku sa mení hneď, netreba OBS reštartovať.
5. Postupne dolaď, kým "Latency" v docku nebude blízko **0** (v rámci pár ms je to v poriadku).
6. Zapamätaj/zapíš si výslednú hodnotu "Latency (ms)" — je to referenčné nastavenie, kým sa
   zvuková reťaz opäť nezmení (nový mikrofón, zmena v Ableton a pod.).

### 7. Ukonči testovací režim

Na dev1:

```
scripts/rig-mode.sh event
```

Toto vráti kameru cam2 späť do normálneho vysielacieho režimu (zhasne testovací QR, zastaví tón).

### 8. KRITICKY DÔLEŽITÉ — pred živým vysielaním vráť mikrofón na MUTED

Kanál, ktorý si v kroku 2 odmutoval v Ableton na mbc, **musí byť pred živým vysielaním znova
STLMENÝ (MUTED)** — inak sa testovací tón môže dostať do živého prenosu.

**Automatická poistka:** pred vysielaním sa dá spustiť krátky test hlasitosti nahrávky
(`volumedetect`, popísaný v `.claude/skills/av-sync/SKILL.md`) — ak by kanál ostal omylom
odmutovaný a v zázname by bol počuť testovací tón, tento test na to upozorní. Ak si nie si
istý, či si kanál vrátil späť, spusti tento test alebo sa opýtaj niekoho, kto vie so skriptami.

## Zhrnutie (rýchla referencia)

1. mbc/Ableton zapnuté?
2. Ableton: mic kanál → **odmutovať** (len na čas merania).
3. `scripts/rig-mode.sh test`
4. OBS na "stream": dock **"Audio Video Sync"** → **Start**.
5. Sleduj **Latency** → cieľ ~0. Ak nie, dolaď **"NDI 2ME PGM" → Properties → Latency (ms)**.
6. `scripts/rig-mode.sh event`
7. Ableton: mic kanál → **späť MUTED** (over si to pred-vysielacím testom, ak si neistý).

## Keď to nefunguje (dock zostáva na pomlčkách)

- **mbc je vypnuté/nedosiahnuteľné** — najčastejšia príčina. mbc mimo vysielania býva bežne
  vypnuté; zapni ho a skús znova.
- **Mic kanál v Ableton je stále stlmený** — skontroluj krok 2.
- **Testovací tón nebeží** — `scripts/rig-mode.sh test` musí sám potvrdiť, že tón hrá; ak
  nahlási chybu, nepokračuj ďalej, kým sa nevyrieši (kontaktuj niekoho, kto vie so skriptami).
- Ak nič z vyššie uvedeného nepomôže, ide pravdepodobne o technický problém — nahlás to (GitHub
  issue v projekte `camera-box`, alebo kontaktuj správcu systému), nesnaž sa to riešiť sám do
  hĺbky.

## Poznámka pre technikov (dôvod, prečo dock číta práve tento zvuk/obraz)

Dock ("Audio Video Sync") sa v OBS pripája priamo na `obs_get_video()` (PROGRAM plátno) a
`obs_get_audio()` (globálny master zvukový mix — presne tá istá zbernica, z ktorej ide
nahrávanie aj stream). Nemá žiadne vlastné nastavenie zdroja — počuje/vidí presne to, čo ide
do živého výstupu, vrátane všetkého, čo je práve nestlmené a smerované do master mixu (teda aj
`mbc`, ak je odmutovaný). To je zámerne PRODUKČNÁ cesta zvuku/obrazu — dock preto meria presne
to, čo vidí/počuje divák, nie žiadnu odbočku (zdrojovo overené: `vendor/av-sync-dock/src/
sync-test-dock.cpp`, `on_start_stop()`). Latencia sa dolaďuje na zdroji `NDI 2ME PGM` (vlastnosť
`Latency (ms)`, interne `genlock_latency_ms_src`), aplikuje sa okamžite za behu (hot-apply, bez
reštartu OBS).

**Stav overenia (2026-07-12):** binárka docku bola prestavaná a nasadená na `strih` aj `stream`
(#698, SHA256 overené na oboch strojoch). Živé zamknutie docku na reálny signál k dnešnému dňu
NEBOLO opätovne overené v tomto behu — `mbc` (10.77.9.232) bolo v čase tejto úlohy nedosiahnuteľné
(pravdepodobne vypnuté mimo vysielania). Tento postup je zostavený zo zdrojového kódu docku a z
doterajších zdokumentovaných zistení (#690, #689, `.claude/skills/av-sync/SKILL.md`) — funkčnosť
KROKOV je správna, ale finálne živé potvrdenie "dock sa naozaj zamkol a ukázal reálne čísla" čaká
na najbližšiu príležitosť, keď bude mbc zapnuté a rig voľný (pozri #690 na GitHube).
