FROM mcr.microsoft.com/devcontainers/rust:2-1-trixie

RUN apt-get update && export DEBIAN_FRONTEND=noninteractive \
   && apt-get -y install clang lld \
   && apt-get autoremove -y && apt-get clean -y
