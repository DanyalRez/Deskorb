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

1. **v0 (fait)** : icône de notification dessinée en direct + infobulle détaillée. Trois styles au choix (clic droit → Icône) :
   **Orbite** (le logo, l'arc se remplit avec la batterie, ou les tokens sans souris), **Jauges** (arc extérieur = batterie,
   arc intérieur = tokens) ou **Pourcentage** (« 87 »). Choix gardé dans `%APPDATA%\Deskorb\icone.txt`. Épinglée hors du menu « icônes cachées », elle est toujours visible.
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
  tokens.rs  fenêtre de 5 h lue dans ~/.claude (blocs, dédoublonnage, `--tokens`) + formats
```

Flux : `WM_TIMER` → `mouse::read()` + `tokens::read()` → nouvelle icône + infobulle → si le panneau est ouvert, `draw()`.
Clic gauche sur l'icône → panneau au-dessus de la barre, à droite ; perte du focus → il se ferme. Clic droit → Quitter.

## Souris : ce que la sonde a trouvé

`cargo run -- --probe` liste les interfaces HID. Dongle détecté : **`3710:5406` « Pulsar 8Kdx Dongle »**, interface `mi_01` avec des collections vendeur :

| Usage page | In / Out / Feature (octets) | Rôle probable |
| --- | --- | --- |
| `FF02` | 17 / 17 / 0 | **canal de commandes** (requête → réponse) : candidat n° 1 pour batterie / DPI / polling |
| `FF06` | 49 / 49 / 0 | transferts plus gros (macros, firmware) |
| `FF04` | 0 / 0 / 8 | Feature report |
| `FF03`, `FF05` | 8 / 0 / 0 | notifications du dongle (ex. changement d'état) |

Prochaine étape : capturer ce que l'app officielle Pulsar envoie sur `FF02` (Wireshark + USBPcap) au moment où elle affiche la batterie, puis reproduire la requête dans `mouse::read()`.

## Reste à faire

- Layout exact du panneau (attend ton schéma) + animations à ressort de DESKTOUCH.
- Protocole batterie / DPI / polling (voir ci-dessus).
- Limite de tokens : heuristique « plus gros bloc passé » (192 M chez toi, % trop bas) ; à remplacer par une valeur fixe.
- Mode minimaliste / détaillé, choix barre ou anneau.
- Icône encre blanche fixe : à adapter si la barre des tâches est en thème clair.
