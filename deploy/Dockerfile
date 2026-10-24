# Runtime image for a Tacit node. The binaries and the WebAssembly are built
# ahead of time (see deploy/README.md), so this image only assembles them.
FROM ubuntu:24.04
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates wget && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY bin/ /app/bin/
COPY web/ /app/web/
COPY start.sh /app/start.sh
RUN chmod +x /app/bin/* /app/start.sh
ENV PORT=8080 DATA_DIR=/data
EXPOSE 8080
CMD ["/app/start.sh"]
