# DESKORB — architecture v0

Batterie de la Pulsar TenZ + quota de tokens, dans la zone de notification, avec un panneau Liquid Glass.

## Stack

Même stack que DESKTOUCH, volontairement : **Rust + `windows-sys` uniquement**, aucune autre dépendance.

| Besoin | Choix | Pourquoi |
| --- | --- | --- |
| Rendu Liquid Glass | `render.rs` de DESKTOUCH copié tel quel (verre calculé sur CPU, réfraction, dispersion, ombre) + fenêtre `WS_EX_LAYERED` / `UpdateLayeredWindow` | Même esthétique au pixel près, 0 % CPU au repos, pas de GPU/WebView à réveiller |
| Flou du fond | `win::capture` (BitBlt de l'écran, fenêtre exclue) + `Back::frost` | Recette DESKTOUCH |
| Texte | GDI `DrawTextW` → calque alpha (`win::text_layer`) | Recette DESKTOUCH |
| Zone de notification | `Shell_NotifyIconW` + icône **régénérée** à chaque relevé (`win::text_icon`) | C'est le « mini-widget » : le % est lisible sans cliquer |
| Lecture souris | API HID de Windows (`SetupDi*` + `HidD_*`), sans pilote | Pas de `hidapi` (C à compiler) pour ~80 lignes |
| Tokens | Transcriptions Claude Code (`~/.claude/projects/**/*.jsonl`), relues en incrémental | Fenêtre de 5 h comme Claude Code ; aucune clé ni réseau |

### Sur la « barre des tâches »

Windows 11 ne permet plus d'insérer un widget *dans* la barre des tâches (les DeskBands sont supprimés). Deux voies réalistes :

1. **v0 (fait)** : icône de notification dessinée en direct + infobulle détaillée. Trois styles au choix (Réglages → Icône) :
   **Orbite** (le logo, l'arc se remplit avec la batterie, ou les tokens sans souris), **Jauges** (arc extérieur = batterie,
   arc intérieur = tokens) ou **Pourcentage** (« 87 »). Choix gardé dans `%APPDATA%\Deskorb\settings.txt`. Épinglée hors du menu « icônes cachées », elle est toujours visible.
2. **Plus tard, si besoin** : une petite fenêtre layered posée *par-dessus* la barre des tâches (technique de TrafficMonitor), pour afficher plus que 2–3 caractères. Plus fragile (thèmes, barre centrée, multi-écran).

## Logo

Deux logos dans la DA de DESKTOUCH (tuile de verre sombre, liseré, pictogramme blanc), dans `assets/` :
`orbite/` et `jauges/` (maître, version petites tailles, pictogramme noir/blanc, PNG 512) et `deskorb-*.ico` (16 → 256 px,
petites tailles en dessous de 32 px). L'exe embarque les deux (`build.rs`, ressources 1 et 2).

## Fichiers

```
src/
  main.rs    fenêtre, icône de notification, minuterie (15 s), panneau flyout
  render.rs  moteur Liquid Glass (copie DESKTOUCH)
  win.rs     texte, capture du fond, icône dynamique, menu (repris de DESKTOUCH shell.rs)
  mouse.rs   énumération HID, état de la souris, `--probe`
  settings.rs réglages (fichier clé=valeur, langue, lancement au démarrage)
  tokens.rs  fenêtre de 5 h lue dans ~/.claude (blocs, dédoublonnage, `--tokens`) + formats
```

Flux : `WM_TIMER` → `mouse::read()` + `tokens::read()` → nouvelle icône + infobulle → si le panneau est ouvert, `draw()`.
Clic gauche sur l'icône → panneau au-dessus de la barre ; sa roue dentée (ou le clic droit sur l'icône) → réglages : thème, icône, panneau détaillé/compact, limite de tokens, actualisation, langue EN/FR, alerte batterie faible, lancement au démarrage. Perte du focus ou Échap → la fenêtre se range.

## Souris : batterie (fait)

Dongle **`3710:5406` « Pulsar 8Kdx Dongle »**. Protocole repris du driver Linux `hid-pulsar` (noyau, famille Kysona/ATK/VXE) :
rapports de 17 octets sur la collection vendeur `FF02` (`mi_01`), envoyés en SET_REPORT (`HidD_SetOutputReport`),
réponse lue en rapport d'entrée. Octet 0 = `0x08`, octet 1 = commande, dernier = `0x55 − somme`.

| Commande | Réponse |
| --- | --- |
| `0x03` état | `[6]` = 1 si la souris est reliée au dongle, 0 si elle dort |
| `0x04` alimentation | `[6]` = batterie %, `[7]` = câble branché (charge), `[8..10]` = tension mV |

La souris s'endort vite quand on ne la touche pas : elle ne répond plus, l'app garde alors la dernière valeur et affiche « en veille ».
`cargo run -- --probe` liste les interfaces et lit la batterie.

## Reste à faire

- Layout exact du panneau (attend ton schéma) + animations à ressort de DESKTOUCH.
- DPI et polling : hors du protocole connu, à relever par capture USB si besoin.
- Dernière batterie connue perdue au redémarrage tant que la souris dort.
- Icône encre blanche fixe : à adapter si la barre des tâches est en thème clair.
