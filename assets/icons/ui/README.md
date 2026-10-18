# UI icons

Small UI icons, taken from [Lucide](https://lucide.dev/)
(<https://github.com/lucide-icons/lucide>), licensed under the ISC License, with Feather-derived icons also under the MIT
License (upstream LICENSE kept verbatim in `licenses/LUCIDE-ISC.txt`). They are not covered by CayenChat's GPL-3.0-only
license. The app icon `assets/icons/cayenchat.svg` is separate.

Rules (decision D039): copy only the SVGs that are used, keep the Lucide file
name, add one row below in the same commit, and do not add icons ahead of use.
Prefer Lucide; only if it has nothing suitable use another source or draw one
and record that here.

| File | Lucide name | Lucide version | Used for |
| --- | --- | --- | --- |
| `plug-zap.svg` | `plug-zap` | 1.56.0 | Toolbar: connect |
| `refresh-cw.svg` | `refresh-cw` | 1.56.0 | Toolbar: reconnect |
| `unplug.svg` | `unplug` | 1.56.0 | Toolbar: disconnect |
| `log-in.svg` | `log-in` | 1.56.0 | Toolbar: join the channel |
| `log-out.svg` | `log-out` | 1.56.0 | Toolbar: leave the channel |
| `message-square-text.svg` | `message-square-text` | 1.56.0 | Toolbar: change the channel topic |
| `sliders-horizontal.svg` | `sliders-horizontal` | 1.56.0 | Toolbar: change the channel mode |
| `user-pen.svg` | `user-pen` | 1.56.0 | Toolbar: change my nickname |
| `chevrons-down.svg` | `chevrons-down` | 1.56.0 | Toolbar: next unread channel |
| `user-search.svg` | `user-search` | 1.56.0 | Toolbar: WHOIS |
| `image-up.svg` | `image-up` | 1.56.0 | Toolbar: upload an image |
| `settings.svg` | `settings` | 1.56.0 | Toolbar: settings |
| `bring-to-front.svg` | `bring-to-front` | 1.56.0 | Toolbar: keep the windows on top (Windows) |
| `search.svg` | `search` | 1.56.0 | Toolbar: search the logs; search bar |
| `chevron-up.svg` | `chevron-up` | 1.56.0 | Search bar: previous match |
| `chevron-down.svg` | `chevron-down` | 1.56.0 | Search bar: next match |
| `circle-x.svg` | `circle-x` | 1.56.0 | Search bar: clear the keyword |
| `x.svg` | `x` | 1.56.0 | Search bar: close |
| `link.svg` | `link` | 1.56.0 | Log: marks a URL when "Show long URLs compactly" is on (the diagonal chain, not `link-2`) |

The files are unchanged from upstream (`stroke="currentColor"`); the toolbar
draws them as a mask in the theme's text color, so they show in both light and
dark. They are compiled into the binary (`toolbar::Icons`), so a new icon is
also added to that list.
