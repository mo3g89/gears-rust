#!/usr/bin/env bash
# Runs once, automatically, on a fresh postgres data volume (the postgres
# image executes every *.sh/*.sql in /docker-entrypoint-initdb.d, in name
# order, only the first time the volume is initialized).
#
# The gears do not create their own databases: libs/toolkit-db/src/options.rs
# only does `opts.database(dbname)` when building connection options -- there
# is no CREATE DATABASE / create_database / database_exists anywhere under
# libs/. A missing per-gear database is a connection error at gear init.
# POSTGRES_DB provisions exactly one database ("postgres"), so the
# other eight named by gears/qa-platform/config/qa-platform-stack.yaml's `dbname:` fields have
# to be created here before the gears container starts.
set -euo pipefail

# Keep this list in sync with the `dbname:` values under `gears:` in
# gears/qa-platform/config/qa-platform-stack.yaml (the config, not this script, is authoritative
# if the two ever disagree):
#   qa_environments, qa_catalog, qa_runs, qa_insights, settings, credstore,
#   resource_group, event_broker
DATABASES=(
    qa_environments
    qa_catalog
    qa_runs
    qa_insights
    settings
    credstore
    resource_group
    event_broker
)

for db in "${DATABASES[@]}"; do
    # `\gexec` is a psql meta-command: it must go through psql's own
    # line-based script parser to be recognized. Fed via `-c`, the whole
    # string is instead sent to the backend as one simple-query string, and
    # Postgres itself chokes on the literal backslash -- confirmed against a
    # throwaway postgres:16 container, `-c` form errors
    # (`syntax error at or near "\""`), stdin/heredoc form succeeds. So this
    # has to go over stdin, not `-c`.
    psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "postgres" <<-SQL
	SELECT 'CREATE DATABASE "${db}"' WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = '${db}')\gexec
	SQL
done
