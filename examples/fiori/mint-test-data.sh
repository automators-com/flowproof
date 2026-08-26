#!/usr/bin/env bash
# suite.yaml's env_from: mints MATERIAL/SUPPLIER/PLANT/NET_PRICE by querying
# the live "Manage Purchasing Info Records" OData service directly.
#
# Replaces an earlier `datamaker sap info-record pick --plant 1010 --format
# env` placeholder that named a CLI command which never existed anywhere in
# the DataMaker tooling — confirmed by searching both the datamaker monorepo
# and datamaker-sap-cli (the actual SAP↔DataMaker bridge tool, whose own
# commands are catalog/extract/transform/connect/push, none of which return
# a single picked record). This script talks straight to SAP instead.
#
# Requires SAP_ODATA_BASE, SAP_USER, SAP_PASSWORD in the environment — same
# credentials the flows themselves use, no separate tool or install needed
# beyond curl + python3.
#
# The reference system's own record data is sparse: the root entity set's
# first several hundred rows (unfiltered) all have an empty Plant field
# (most info records here are purchasing-org-level, not plant-specific), so
# an unfiltered "first row" pick returns unusable data. Querying server-side
# with $filter=Plant eq '<PLANT_FILTER>' finds the real plant-scoped subset.
set -euo pipefail

PLANT_FILTER="${PLANT_FILTER:-1010}"
SERVICE="MM_PUR_INFO_RECORDS_MANAGE_SRV"
ENTITY="C_PurInfoRecordWithOrg"

# SAP_ODATA_VERIFY_SSL=false is the reference system's own posture (it
# presents a certificate curl/python don't trust by default) — not a
# recommendation, just what this specific reference system needs.
CURL_INSECURE_FLAG=""
if [ "${SAP_ODATA_VERIFY_SSL:-true}" = "false" ]; then
  CURL_INSECURE_FLAG="-k"
fi

curl -sf $CURL_INSECURE_FLAG -u "${SAP_USER}:${SAP_PASSWORD}" \
  "${SAP_ODATA_BASE}/${SERVICE}/${ENTITY}?\$format=json&\$top=5&\$filter=Plant%20eq%20'${PLANT_FILTER}'" \
| python3 -c '
import json, sys

rows = json.load(sys.stdin)["d"]["results"]
usable = [r for r in rows if r.get("Material") and r.get("Supplier") and r.get("NetPriceAmount")]
if not usable:
    sys.exit("no usable info record found for Plant=" + repr(sys.argv[1]) + " (got " + str(len(rows)) + " rows, none with Material+Supplier+NetPriceAmount all set)")

r = usable[0]
material = r["Material"]
supplier = r["Supplier"]
plant = r["Plant"]
net_price = r["NetPriceAmount"]
print("MATERIAL=" + material)
print("SUPPLIER=" + supplier)
print("PLANT=" + plant)
print("NET_PRICE=" + str(net_price))
' "$PLANT_FILTER"
