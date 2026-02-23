# Ethos — Database Migrations

## Prerequisites

### 1. PostgreSQL 17 (installed 2026-02-22)

Running: **PostgreSQL 17.7** on Ubuntu 25.10 (questing). Installed from standard Ubuntu repos.

```bash
sudo apt install postgresql-17 postgresql-contrib-17 postgresql-17-pgvector
sudo systemctl enable --now postgresql
```

### 2. pgvector extension (v0.8.0 — supports HNSW ✅)

**Important:** pgvector must be created as superuser, not as the ethos user.

```bash
# Run as postgres superuser — NOT as the ethos user
sudo -u postgres psql -d ethos -c "CREATE EXTENSION IF NOT EXISTS vector;"
```

The `pg_trgm` extension can be created by the ethos user normally.

---

## Initial Setup

Run once to create the database user and database:

```bash
# Create the ethos user
sudo -u postgres createuser --no-superuser --no-createdb --no-createrole ethos
sudo -u postgres psql -c "ALTER USER ethos WITH PASSWORD 'ethos_dev';"

# Create the ethos database
sudo -u postgres createdb --owner=ethos ethos
sudo -u postgres psql -d ethos -c "GRANT ALL PRIVILEGES ON DATABASE ethos TO ethos;"
sudo -u postgres psql -d ethos -c "GRANT ALL ON SCHEMA public TO ethos;"
```

---

## Running Migrations

From the project root (`/home/revenantpulse/Projects/ethos`):

```bash
# Run migration 001 (initial schema)
psql -U ethos -d ethos -h localhost -f migrations/001_initial_schema.sql

# Verify tables
psql -U ethos -d ethos -h localhost -c "\dt"

# Verify vector column and HNSW index
psql -U ethos -d ethos -h localhost -c "\d memory_vectors"

# Count all indexes
psql -U ethos -d ethos -h localhost -c "SELECT COUNT(*) FROM pg_indexes WHERE schemaname = 'public';"
```

Expected output from `\dt`:
```
             List of relations
 Schema |         Name          | Type  | Owner
--------+-----------------------+-------+-------
 public | episodic_traces       | table | ethos
 public | memory_graph_links    | table | ethos
 public | memory_vectors        | table | ethos
 public | semantic_facts        | table | ethos
 public | sessions              | table | ethos
 public | workflow_memories     | table | ethos
```

---

## Reset (Dev Only)

To wipe and re-run:

```bash
sudo -u postgres psql -c "DROP DATABASE IF EXISTS ethos;"
sudo -u postgres createdb --owner=ethos ethos
sudo -u postgres psql -d ethos -c "GRANT ALL PRIVILEGES ON DATABASE ethos TO ethos;"
sudo -u postgres psql -d ethos -c "GRANT ALL ON SCHEMA public TO ethos;"
psql -U ethos -d ethos -h localhost -f migrations/001_initial_schema.sql
```

---

## Connection String

```
postgresql://ethos:ethos_dev@localhost:5432/ethos
```

Set in `ethos.toml`:
```toml
[database]
url = "postgresql://ethos:ethos_dev@localhost:5432/ethos"
max_connections = 10
```

---

## Notes

- `vector(768)` dimension is **fixed** at creation — changing it requires rebuilding the table
- HNSW index requires pgvector >= 0.5.0
- `ethos_dev` password is for development only — production uses a secrets manager
- The `pg_trgm` extension enables fast trigram text search on `content` and `statement` columns
