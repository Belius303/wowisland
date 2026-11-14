# Privacy policy

*Last updated: November 14, 2026 · [Français plus bas](#politique-de-confidentialité)*

WowIsland is a desktop app that runs entirely on your PC. **The author does not collect, receive or store any of your data.** There is no account, no analytics, no telemetry and no server run by the project.

## What stays on your PC

Settings, logs, caches and downloaded tools are stored in `%USERPROFILE%\.dynamicisland`. Passwords and tokens you enter (Gmail app passwords, Spotify and Discord tokens) are encrypted with Windows DPAPI, so only your Windows session can read them. Uninstalling is deleting the exe and that folder.

## Services WowIsland talks to

WowIsland only contacts a service when you use the feature that needs it. Each connection goes **directly from your PC to that service**; nothing goes through the project. These services have their own privacy policies.

| Feature | Service | What is sent |
|---|---|---|
| Gmail codes | Google (`imap.gmail.com`) | Your Gmail address and app password, to read new mails. Mails are never modified or marked as read. |
| Synced lyrics | [LRCLIB](https://lrclib.net) | Title, artist, album and duration of the track playing |
| Spotify volume | Spotify (`accounts.spotify.com`, `api.spotify.com`) | Sign-in with your Spotify account (OAuth), then volume / playback commands |
| Discord voice | Discord (`discord.com`, `cdn.discordapp.com`) and the Discord app on your PC | Sign-in with your own Discord developer app (OAuth); avatars are loaded from Discord's CDN |
| Game detection | Discord (`discord.com`, `cdn.discordapp.com`) | Download of Discord's public list of detectable games and their icons (no personal data) |
| App icons | Apple (`itunes.apple.com`) | The name of the app that sent a notification, to find its icon |
| Downloads | The site of the link you paste (YouTube, TikTok…), [Deezer API](https://developers.deezer.com) for music links, GitHub to fetch yt-dlp / ffmpeg / Deno | The link you paste; track titles and artists to match music links. Only if you turn on **Browser cookies** (off by default): yt-dlp reads that browser's cookies on your PC at download time and sends each site only its own cookies, like the browser would. They are never copied or kept by WowIsland. |
| Send to phone | [LocalSend](https://localsend.org) on your own phone | The file you choose, over your local Wi-Fi only |
| LoL coach | Riot's local API on your PC, [OP.GG](https://op.gg), Data Dragon (`ddragon.leagueoflegends.com`) | Champion names and your role, to fetch builds and tips |
| Claude Code | Anthropic (`api.anthropic.com`), only if you use the Claude Code feature | Your usage-quota request, with your local Claude Code sign-in (read in memory, never stored or logged) |

Notifications from Windows and from your iPhone (through Microsoft Phone Link) are read locally and shown in the island; they are not sent anywhere.

## Contact

Questions or concerns: open an issue at https://github.com/Belius303/wowisland/issues.

---

# Politique de confidentialité

*Dernière mise à jour : 14 novembre 2026*

WowIsland est une application de bureau qui tourne entièrement sur ton PC. **L'auteur ne collecte, ne reçoit et ne stocke aucune de tes données.** Pas de compte, pas de statistiques, pas de télémétrie, pas de serveur du projet.

- **Sur ton PC :** réglages, journal, caches et outils sont dans `%USERPROFILE%\.dynamicisland`. Les mots de passe et jetons (mots de passe d'application Gmail, jetons Spotify / Discord) sont chiffrés avec DPAPI de Windows. Pour tout effacer, supprime l'exe et ce dossier.
- **Services tiers :** WowIsland ne contacte un service que lorsque tu utilises la fonction qui en a besoin, toujours directement depuis ton PC (voir le tableau ci-dessus : Gmail, LRCLIB, Spotify, Discord, Apple pour les icônes d'applis, sites de téléchargement, Deezer, GitHub, LocalSend en Wi-Fi local, OP.GG, Data Dragon, Anthropic). Chacun a sa propre politique de confidentialité.
- **Cookies du navigateur** (téléchargeur, désactivé par défaut) : si tu l'actives, yt-dlp lit les cookies du navigateur choisi sur ton PC au moment du téléchargement et n'envoie à chaque site que ses propres cookies, comme le ferait le navigateur. WowIsland ne les copie ni ne les garde.
- **Notifications** Windows et iPhone (via Mobile connecté) : lues sur ton PC, jamais envoyées ailleurs.

Contact : https://github.com/Belius303/wowisland/issues
