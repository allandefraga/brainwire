# Seu primeiro grafo com Brainwire

[Read this guide in English](getting-started.md)

Neste guia, você vai rodar um servidor local com Docker Compose, gravar dois
nós e uma relação em Python e explorar o resultado no terminal.

## 1. Inicie o servidor

Use o Git e o Docker com Compose. Baixe uma versão publicada e use a imagem
com a mesma tag. Os comandos de download e a versão atual estão no
[início rápido do README](../README.md#quickstart). No macOS, se já estiver
usando Colima, defina `DOCKER_CONTEXT=colima` no terminal.

Depois de entrar na pasta do repositório e definir `BRAINWIRE_IMAGE`, execute:

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  up --no-build --pull always --wait --wait-timeout 900
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  exec -T brainwire brainwire probe --mode ready
```

A demonstração cria o grafo `demo` e abre `bolt://127.0.0.1:7687`.
Você pode baixar a imagem pública sem login. Se a porta estiver ocupada,
execute `export BRAINWIRE_BIND_PORT=17687` antes de iniciar e use a porta
`17687` na conexão Python. O terminal roda dentro do container e continua
usando a porta interna `7687`.

Use dados fictícios em uma máquina e uma rede de containers de confiança.
A demonstração funciona sem autenticação e expõe a porta no endereço local
da máquina. Para dados reais ou acesso compartilhado, configure credenciais
e transporte criptografado. [Como proteger o servidor](protected-containers.md) ·
[Transporte protegido](protected-transport.md)

## 2. Grave e consulte dados em Python

Use Python 3.9 ou mais recente. Na pasta do repositório, execute
`python3 scripts/setup_python.py` para instalar o cliente local e o driver
Bolt em `.venv-brainwire`. Se esse ambiente já existir, use
`--venv /novo/caminho` e ajuste o caminho do Python abaixo.

Salve este código como `first_graph.py` e execute
`.venv-brainwire/bin/python first_graph.py`.

```python
from brainwire import connect

with connect("bolt://127.0.0.1:7687", "demo", auth=None) as graph:
    graph.upsert_node("Customer", "customer-1", {"name": "Ada"})
    graph.upsert_node("Segment", "segment-1", {"name": "beta"})
    graph.upsert_edge(
        ("Customer", "customer-1"), "IN_SEGMENT", ("Segment", "segment-1")
    )
    rows = graph.run_cypher("""
        MATCH (c:Customer)-[:IN_SEGMENT]->(s:Segment)
        RETURN c.name AS customer, s.name AS segment
    """)
    print(rows)
```

Você verá o cliente e o segmento ligados pela relação.

```text
[{'customer': 'Ada', 'segment': 'beta'}]
```

Cada operação do cliente tem sua própria transação. Se a conexão cair sem
resposta ao confirmar uma gravação, verifique se ela foi aplicada antes de
tentar de novo. [Operações e lotes em Python](python-client.md)

## 3. Explore o grafo no terminal

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  exec brainwire brainwire shell --connect 127.0.0.1:7687 --graph demo \
  --node-label Customer --entity-id customer-1 \
  --edge-type IN_SEGMENT --target-label Segment --lang pt-BR
```

Use `Tab` ou as teclas de `1` a `4` para mudar de aba. Pressione `?` para
ver os atalhos, `r` para atualizar e `q` para sair. O terminal conectado é
somente para leitura. A aba Grafo mostra uma amostra das relações de saída
do tipo escolhido. [Guia do terminal](connected-status.md)

## 4. Pare o servidor e mantenha o grafo

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml down
```

Os dados ficam no volume Docker. Para reabrir o grafo, rode o comando `up`
do primeiro passo. Use `down --volumes` somente para apagar os dados da
demonstração. Para guardar uma cópia independente, crie e verifique um
checkpoint. [Backups e restauração](checkpoints.md)

Para continuar, os guias técnicos estão em inglês.

- [Consultas Cypher e modo local](local-cypher.md)
- [Definição de nós e relações](online-catalog.md)
- [Memória de agentes e fontes](agent-context-quickstart.md)
- [Solução de problemas no Docker](container.md)
