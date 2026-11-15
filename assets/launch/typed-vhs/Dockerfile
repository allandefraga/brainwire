FROM ghcr.io/allandefraga/brainwire@sha256:4c8dbfde91a66d6cee5ebae2eeaab4d164ed13d097ca225fc9c477df0f18df3b AS brainwire
FROM ghcr.io/charmbracelet/vhs@sha256:ea49a6a1c529be83153e88321892b5585964418f0b9055e8c1e0d732194234a3
COPY --from=brainwire /usr/local/bin/brainwire /usr/local/bin/brainwire
