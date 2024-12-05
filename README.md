# Race DNS Proxy

> ⚠️ **Experimental**: This project is currently in experimental stage and under active development.

A high-performance DNS proxy that races multiple DNS-over-HTTPS (DoH) providers to get the fastest response.

## Features

- Concurrent DNS queries to multiple providers
- Automatic failover and retry
- DNS-over-HTTPS (DoH) support
- Smart response selection based on speed and status
- Built with Rust for high performance and reliability
- Configurable DNS providers via TOML configuration

## Configuration

Create or modify `config.toml` to configure DNS providers:

```toml
[providers.alidns-doh]
addr = "223.5.5.5:443"
hostname = "dns.alidns.com"

[providers.dnspod-doh]
addr = "1.12.12.12:443"
hostname = "doh.pub"
```

## Usage

Run the proxy server:

```bash
race-dns-proxy [OPTIONS]
```

### Command Line Options

```
Options:
  -p, --port <PORT>      DNS server listening port [default: 5653]
      --log <LOG>        Log filepath
  -c, --config <CONFIG>  Configuration file path [default: config.toml]
  -h, --help             Print help
  -V, --version          Print version
```

The server will listen for DNS queries and forward them to configured DoH providers.

## License

MIT License
