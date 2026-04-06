# Xylem Ecosystem - Dev Environment Setup Guide

This document contains everything needed to instruct an AI assistant to set up a new Xylem development environment from scratch on an Ubuntu/Linux server.

## 1. SSH Config & Git Authentication
To pull these repositories, the server will need SSH keys configured for each of the following aliases in `~/.ssh/config`. An AI should be instructed to generate unique SSH keys for each of these aliases and add them to Github.

```ssh-config
Host github-xylem-backend
    HostName github.com
    User git
    IdentityFile ~/.ssh/id_rsa_xylem_backend

Host github-xylem-frontend
    HostName github.com
    User git
    IdentityFile ~/.ssh/id_rsa_xylem_frontend

Host github-xylem-simulation
    HostName github.com
    User git
    IdentityFile ~/.ssh/id_rsa_xylem_simulation

Host github-xylem-sme-analyzer
    HostName github.com
    User git
    IdentityFile ~/.ssh/id_rsa_xylem_sme_analyzer

Host github-xylem-geo
    HostName github.com
    User git
    IdentityFile ~/.ssh/id_rsa_xylem_geo

Host github-xylem-admin
    HostName github.com
    User git
    IdentityFile ~/.ssh/id_rsa_xylem_admin

Host github-xylem-internal-docs
    HostName github.com
    User git
    IdentityFile ~/.ssh/id_rsa_xylem_internal_docs
```

## 2. Codebases & Git Remotes

Here are the respective clone commands for each codebase:

| Project | Directory | SSH Git Remote |
|---------|-----------|----------------|
| **Backend** | `xylem-backend` | `git@github-xylem-backend:harshitaneja/xylem-backend.git` |
| **Frontend** | `xylem-frontend` | `git@github-xylem-frontend:harshitaneja/xylem-frontend-next.git` |
| **Simulation** | `xylem-simulation-backend` | `git@github-xylem-simulation:harshitaneja/xylem-simulation-backend.git` |
| **SME Analyzer** | `xylem-sme-analyzer` | `git@github-xylem-sme-analyzer:harshitaneja/xylem-sme-analyzer.git` |
| **Geo** | `xylem-geo` | `git@github-xylem-geo:harshitaneja/xylem-geo.git` |
| **Admin** | `xylem-admin` | `git@github-xylem-admin:harshitaneja/xylem-admin-frontend.git` |
| **Internal Docs** | `xylem-internal-docs` | `git@github-xylem-internal-docs:harshitaneja/xylem-internal-docs.git` |

*(Note: AI should run `git clone <remote> <directory>` to pull these down into `/var/www/xylem`)*

## 3. System-Level Dependencies
An AI must install the following system dependencies before attempting to build or run the infrastructure:

* **Node.js** (v24 installed via `nvm`) & Package Managers (`npm` / `yarn` / `pnpm`)
  * Ensure AI installs Node Version Manager (`nvm`) first and runs `nvm install 24` & `nvm use 24`.
* **Python 3.10+** & **uv** (Python package installer and resolver)
* **PostgreSQL** & **PostGIS** extension (Required for the `xylem-backend`)
* **GDAL Tools**: `gdal-bin` and `python3-gdal` (Required heavily by the backend for spatial operations like `ogr2ogr`, `gdalwarp`, and `gdal2xyz.py`).
* **GNU LibreDWG**: May need to be built and installed from source (provides `dwgread` for DWG parsing fallback).
* **NGINX** (Web server & reverse proxy layer)
* **PM2** (Node.js process manager, install globally via `npm i -g pm2`)
* **Certbot** (For configuring SSL on NGINX domains via LetsEncrypt)
* **ODAFileConverter & Xvfb**: Along with `libxkbcommon`. This is critically required by the `xylem-sme-analyzer` for headless DWG to DXF conversions.
* **Puppeteer Dependencies**: System libraries required by headless Chrome (libnss3, libcups2, libatk, etc.), necessary for the Backend module to generate PDF reports.
* **Tippecanoe / MBTiles tools**: Required by `xylem-geo` for map tile ingestion and generation processes.

## 4. Environment & Process Management (PM2)

The system relies on an `ecosystem.config.js` file at the root to orchestrate local processes. 

| Application | Local Port | Run Command / Script | Additional Context |
|-------------|------------|----------------------|--------------------|
| `xylem-frontend` | 4801 | `npm run start` | Handled via PM2, requires `PORT: 4801` in env block. |
| `xylem-backend` | 4900 | `node dist/src/main` | Runs via PM2, heavily reliant on `.env` (DB keys). Port inferred from Nginx config. |
| `xylem-sme-analyzer` | 4802 | `uv run uvicorn app.main:app` | `... --host 0.0.0.0 --port 4802 --workers 2` |
| `xylem-simulation-backend` | 4902 | `uv run uvicorn src.api.main:app` | `... --host 0.0.0.0 --port 4902` |
| `xylem-geo` | 4830 | `node dist/server.js` | Handled via PM2, requires `BASE_URL: "https://geo.xylem.city"` and `PORT: 4830`. |

## 5. NGINX Routing Configuration

The server expects NGINX to handle caching, SSL termination, and reverse proxying to the local ports above with increased timeouts (~300s) for heavy computations like PDF generation and file processing.

* **`xylem.city` & `www.xylem.city`** (Frontend) -> `proxy_pass http://127.0.0.1:4801`
* **`api.xylem.city`** (Backend REST API) -> `proxy_pass http://127.0.0.1:4900`
* **`sme.xylem.city`** (SME Analyzer) -> `proxy_pass http://127.0.0.1:4802`
* **`geo.xylem.city`** (Geo Service Engine) -> `proxy_pass http://127.0.0.1:4830`
* **`models.xylem.city`** (Simulation Engine/Models) -> `proxy_pass http://127.0.0.1:4902`
* **`admin.xylem.city`** (Admin UI) -> Completely statically served. Root points directly to `/var/www/xylem/xylem-admin/dist` and handles routing via `try_files $uri $uri/ /index.html;`

### AI Setup Instructions for NGINX:
1. Create individual server block files in `/etc/nginx/sites-available/` for each domain logic above.
2. Ensure WebSocket upgrade headers are passed appropriately (`Upgrade $http_upgrade; Connection 'upgrade';`).
3. Set `client_max_body_size 100M;` for blocks receiving CAD/DWG uploads (like SME config or API).
4. Create Symlinks to `/etc/nginx/sites-enabled/`.
5. Provision SSL certs with `certbot --nginx`.
