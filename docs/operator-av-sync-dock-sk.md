# Postup: kontrola A/V synchronizácie (dock "Audio Video Sync") — pre obsluhu

Jednostránkový postup pre obsluhu prenosu (nie pre vývojára). Slúži na overenie, či je obraz
a zvuk vo vysielaní časovo zarovnaný (žiadne "posunuté pery"), a na dolaďovanie, ak nie je.

## Čo to je

V OBS na počítači **stream** (10.77.9.204) je panel ("dock") s názvom **"Audio Video Sync"**.
Keď sa spustí, počúva PRESNE ten istý zvuk a obraz, ktorý ide do živého vysielania (program +
finálny zvukový mix) — nie žiadnu odbočku ani skúšobný signál naviac. Ak dock ukáže hodnotu
"Latency" blízku 0, obraz a zvuk sú zarovnané.

**Od tohto ticketu (#926) sa dolaďuje SÁM, kým beží testovací signál** — dock nastavenie
"NDI 2ME PGM → Latency (ms)" priamo upravuje, aby "Latency" nikdy neostala na zápornej hodnote
("zvuk predbieha obraz" — to je zakázaný stav, zvuk je vo fyzike vždy pomalší ako obraz). Manuálne
doladenie (krok 6 nižšie) je teraz len ZÁLOŽNÝ postup, keby automatika z nejakého dôvodu
nefungovala.

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
Ak je už niekde pripnutý (napr. v bočnom paneli, prípadne skrytý za iným panelom ako záložka —
pozri si aj susedné záložky), len ho nájdi a klikni naň.

**Od #690 dock meria AUTOMATICKY** — sám sa spustí hneď po naštartovaní OBS (nie je to už
zabudnuteľné tlačidlo, ktoré treba po každom reštarte OBS ručne odklikať). Zvyčajne teda stačí
dock otvoriť a rovno sledovať hodnoty (krok 5) — netreba klikať Start. Ak by niekto meranie
predtým ručne zastavil (tlačidlo ukazuje "Start", nie "Stop"), klikni naň.

### 5. Sleduj hodnoty (Start klikni len ak dock ešte nemeria)

Ak tlačidlo v docku ukazuje "Stop", dock už meria (viď krok 4) — over rovno hodnoty. Ak ukazuje
"Start", klikni naň. Do pár sekúnd by sa mali začať napĺňať polia:

| Pole v docku | Čo znamená |
|---|---|
| **Status** (hore, veľkým písmom) | Jednoduchý stav: "Measuring..." (ešte sa nezamklo), "Locked -- holding sync" (zamklo, drží sa v sync), "No test signal -- holding last correction" (testovací signál nebeží — hodnota je zamrznutá na poslednej korekcii). |
| **Latency** | O koľko je zvuk a obraz mimo seba (v ms). Cieľ: blízko **0**. Kým beží testovací signál a stav je "Locked", dock toto SÁM naťahuje smerom k 0/mierne kladnej hodnote — netreba nič robiť ručne. |
| (pod Latency) | "Audio lagged" = zvuk ide neskôr (v poriadku); "Audio early" = zvuk predbieha obraz (dočasný prechodný stav počas doťahovania — v ustálenom stave sa toto už nemá objavovať). |
| **Index / Audio Index / Video Index** | Interné čísla, podľa ktorých dock páruje obraz a zvuk — netreba im rozumieť, len že sa MENIA (nie samé pomlčky). |
| **Audio Frequency** | Nameraná frekvencia testovacieho tónu — potvrdenie, že dock naozaj počuje ten správny tón. |
| **Audio Resampling (ASRC)** sekcia | Transparentnosť dorovnávania rýchlosti zvuku (nezávislé od Latency vyššie): **State** (ON/OFF), **Estimated drift** / **Applied correction** (koľko odchýlky sa práve meria/opravuje, v ppm), **Manual trim** (ručná jemná úprava, tlačidlá `-`/`+`). Za normálnych okolností sa do toho nemá zasahovať — je to tu pre transparentnosť ("vidieť, čo sa deje"), nie pre bežné doladenie. |

**Ak po ~10-15 sekundách zostávajú samé pomlčky `-`:** dock nič nepočuje/nevidí. Over znova
krok 1-3 (mbc zapnuté? kanál odmutovaný? testovací tón naozaj beží?) — pozri aj sekciu
"Keď to nefunguje" nižšie.

### 6. Ak "Latency" po dlhšom čase stále nie je blízko 0 — ZÁLOŽNÝ manuálny postup

Automatika (vyššie) by mala doladiť sama, kým beží testovací signál a dock ukazuje "Locked". Ak
by to z nejakého dôvodu nefungovalo (napr. hodnota je mimo hardvérového rozsahu 3-2000 ms a
automatika sa nevie dostať ďalej), dá sa doladiť aj ručne:

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
Dock po tomto prejde do stavu "No test signal -- holding last correction" a hodnota "NDI 2ME PGM
Latency (ms)", ktorú si automatika doladila v kroku 5, ostáva NATRVALO — dock ju už ďalej sám
neupravuje (žiadne "naháňanie" driftu podľa bežného programového zvuku počas živého vysielania).

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
- **Pre technikov (#690):** dock od tejto verzie píše do OBS logu (na "stream") každých ~10 s
  jeden riadok `av-sync-dock: diag video_frames=... video_decoded=...(...%) audio_samples=...
  preambles=... crc_ok=... crc_fail=... ring_hit=... ring_miss=... locked=...` — z neho sa dá bez
  prístupu k rigu vyčítať, či problém je (a) obraz — kamera vôbec nevidí QR kód (nízke
  `video_decoded`%), (b) zvuk — demodulátor nič nepočuje (`preambles=0`, zvuková vetva/hlasitosť),
  (c) zvuk počuje, ale je to šum (`preambles>0`, `crc_ok=0`), alebo (d) zvuk dekóduje správne, ale
  nepáruje sa s obrazom / nezamkne (`crc_ok>0`, `locked=no`).

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

**Stav overenia (2026-08-01):** živý test toho dňa ukázal `Audio Frequency = 442 Hz` (tón sa
detegoval), ale `Audio Index`/`Latency` ostali na pomlčkách — a dock bol navyše treba ručne
odklikať (po reštarte OBS zo dňa spal). Dve veci sa opravili priamo v tomto ticket-e:
1. **Dock teraz meria AUTOMATICKY** po štarte OBS (viď krok 4 vyššie) — už netreba spoliehať sa na
   to, že si niekto všimne, že treba kliknúť Start.
2. **Pridaná diagnostika do OBS logu** (viď sekcia "Keď to nefunguje" vyššie) — ukáže PRESNE, kde
   reťaz zvuk→obraz zlyháva (obraz nevidí QR / zvuk nič nepočuje / zvuk počuje šum / zvuk dekóduje
   ale nezamkne), bez potreby ďalšieho živého behu na diagnostiku.

Zostáva otvorené (sledované samostatne, #921): kamera pri 4-min behu 1.8.2026 dekódovala len ~2 %
snímok QR kódu (`Video Index ... 98% missed`) — to je vec obrazu/kamery/kompresie, nie tohto
docku; opravu treba robiť podľa reálne zachyteného snímku, nie naslepo. Postup KROKOV vyššie je
funkčne overený zo zdrojového kódu; finálne živé potvrdenie "dock sa zamkol a ukázal reálne číslo
Latency" čaká na ďalší živý beh s bežiacim mbc (pozri #690 na GitHube).

**Automatická korekcia (#926, pridané 2026-08-01, doladené 2026-08-01 po hĺbkovej revízii):** kým
dock ukazuje stav "Locked", sám naťahuje "NDI 2ME PGM → Latency (ms)" tak, aby výsledné "Latency"
nikdy neostalo záporné ("Audio early" ako trvalý stav je fyzikálne nezmyselné — zvuk je vždy
pomalší ako obraz). Cieľom NIE JE presne 0ms — meranie má bežný šum ~desiatky ms, takže korekcia
cieli na malú bezpečnú rezervu nad nulou (odvodenú od aktuálnej rozptýlenosti merania, min. 1ms),
aby ju obyčajný šum merania nevrátil naspäť do zápornej hodnoty. Krok po kroku sa hodnota mení
najviac o pár ms naraz (aby to nebolo skokové/počuteľné), takže po veľkej odchýlke to chvíľu trvá,
kým sa doladí — to je normálne. Keď testovací signál prestane bežať (krok 7), táto korekcia sa
NATRVALO zamkne a dock ju už neupravuje počas živého vysielania. Zdrojovo:
`vendor/av-sync-dock/src/camera-box-audio.hpp` (`CbDockLockCorrector`) +
`src/av_sync_dock.rs` (`DockLockCorrector`, referenčná Rust implementácia s testami).
