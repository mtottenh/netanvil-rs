#!/usr/bin/env bash
#
# Generate mTLS certificates for netanvil-rs leader/agent communication.
#
# Creates:
#   certs/ca.crt, certs/ca.key         - Certificate Authority
#   certs/leader.crt, certs/leader.key - Leader client certificate
#   certs/agent.crt, certs/agent.key   - Agent server certificate
#
# Usage:
#   ./scripts/gen-certs.sh
#   ./scripts/gen-certs.sh certs/    # custom output directory
#
# Then:
#   netanvil-cli agent --listen 9090 --tls-ca certs/ca.crt --tls-cert certs/agent.crt --tls-key certs/agent.key
#   netanvil-cli leader --workers localhost:9090 --tls-ca certs/ca.crt --tls-cert certs/leader.crt --tls-key certs/leader.key ...

set -euo pipefail

OUTDIR="${1:-certs}"
DAYS=3650  # 10 years for dev certs

mkdir -p "$OUTDIR"

echo "==> Generating CA..."
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
    -days "$DAYS" -nodes \
    -keyout "$OUTDIR/ca.key" -out "$OUTDIR/ca.crt" \
    -subj "/CN=netanvil-rs CA" \
    2>/dev/null

echo "==> Generating agent certificate..."
openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
    -nodes -keyout "$OUTDIR/agent.key" -out "$OUTDIR/agent.csr" \
    -subj "/CN=netanvil-rs agent" \
    2>/dev/null

openssl x509 -req -in "$OUTDIR/agent.csr" \
    -CA "$OUTDIR/ca.crt" -CAkey "$OUTDIR/ca.key" -CAcreateserial \
    -days "$DAYS" -out "$OUTDIR/agent.crt" \
    -extfile <(printf "subjectAltName=DNS:localhost,IP:127.0.0.1") \
    2>/dev/null

echo "==> Generating leader certificate..."
openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
    -nodes -keyout "$OUTDIR/leader.key" -out "$OUTDIR/leader.csr" \
    -subj "/CN=netanvil-rs leader" \
    2>/dev/null

openssl x509 -req -in "$OUTDIR/leader.csr" \
    -CA "$OUTDIR/ca.crt" -CAkey "$OUTDIR/ca.key" -CAcreateserial \
    -days "$DAYS" -out "$OUTDIR/leader.crt" \
    2>/dev/null

# Clean up CSRs
rm -f "$OUTDIR"/*.csr "$OUTDIR"/*.srl

echo ""
echo "Certificates generated in $OUTDIR/:"
ls -la "$OUTDIR/"
echo ""
echo "Agent:"
echo "  netanvil-cli agent --listen 9090 \\"
echo "    --tls-ca $OUTDIR/ca.crt --tls-cert $OUTDIR/agent.crt --tls-key $OUTDIR/agent.key"
echo ""
echo "Leader:"
echo "  netanvil-cli leader --workers localhost:9090 --url http://example.com --rps 100 --duration 10s \\"
echo "    --tls-ca $OUTDIR/ca.crt --tls-cert $OUTDIR/leader.crt --tls-key $OUTDIR/leader.key"
