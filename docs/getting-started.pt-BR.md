# Seu primeiro grafo com Brainwire

[Read this guide in English](getting-started.md)

Brainwire é um banco de grafos de código aberto para dados conectados. Neste
guia, você liga um projeto a uma decisão e consulta essa relação com Cypher.

Use Homebrew para abrir um grafo local e rodar Cypher direto no terminal do
Mac. Os comandos usam apenas o executável `brainwire`.

Use Docker Compose quando quiser o servidor em um contêiner Linux, com um
volume persistente e um endereço Bolt para conectar aplicações. O pacote
Homebrew também inclui `brainwire serve` caso você prefira rodar o servidor
direto no Mac. [Configuração do servidor nativo](serve.md)

## Homebrew no macOS

Instale pelo tap do Brainwire e selecione o binário instalado pelo Homebrew
para esta sessão do terminal.

```sh
brew install allandefraga/brainwire/brainwire
brainwire_prefix="$(brew --prefix allandefraga/brainwire/brainwire)"
export PATH="$brainwire_prefix/bin:$PATH"
brainwire --version
```

Crie um grafo em um diretório temporário. A ontologia de exemplo define
`Project` e `Decision` como tipos de nó e `HAS_DECISION` como relação entre
eles. Grave os dois nós e consulte o projeto com a decisão ligada a ele.

```sh
demo_dir="$(mktemp -d)"
ontology="$brainwire_prefix/share/brainwire/company-ontology.json"
brainwire cypher --database "$demo_dir/graph" --ontology "$ontology" \
  --query "CREATE (:Project {entity_id:'release', name:'Release'})-[:HAS_DECISION]->(:Decision {entity_id:'retry', title:'Retry failed requests'})"
brainwire cypher --database "$demo_dir/graph" \
  --query "MATCH (p:Project)-[:HAS_DECISION]->(d:Decision) RETURN p.name AS project, d.title AS decision"
```

O campo `rows` retorna `[["Release", "Retry failed requests"]]`. As duas
operações abrem o mesmo grafo em `$demo_dir/graph`. Para guardar os dados,
escolha uma pasta permanente fora da instalação do Homebrew, como
`$HOME/.local/share/brainwire/meu-grafo`.

Cada diretório RocksDB pode ser aberto por um processo por vez. Se um servidor
já abriu aquele grafo, os clientes acessam os dados pelo Bolt. A consulta local
com `brainwire cypher --database` abre o diretório diretamente. [Guia do Homebrew](homebrew.md)

## Docker Compose

### 1. Inicie o servidor

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
`17687` nos clientes que rodam na sua máquina. O terminal roda dentro do
contêiner e continua usando a porta interna `7687`.

Use dados fictícios em uma máquina e uma rede de containers de confiança.
A demonstração funciona sem autenticação e expõe a porta no endereço local
da máquina. Para dados reais ou acesso compartilhado, configure credenciais
e transporte criptografado. [Como proteger o servidor](protected-containers.md) ·
[Transporte protegido](protected-transport.md)

### 2. Explore o esquema no terminal

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  exec brainwire brainwire shell --connect 127.0.0.1:7687 --graph demo \
  --lang pt-BR
```

Use `Tab` ou as teclas de `1` a `4` para mudar de aba. Pressione `?` para
ver os atalhos, `r` para atualizar e `q` para sair. O terminal conectado é
somente para leitura. Comece pela aba Ontologia para ver os tipos de nós e
relações do exemplo. Para ver os dados na aba Grafo, informe também o nó inicial
e o tipo de relação ao iniciar a interface. [Guia do terminal](connected-status.md)

### 3. Pare o servidor e mantenha o grafo

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml down
```

Os dados ficam no volume Docker. Para reabrir o grafo, rode o comando `up`
do primeiro passo. Use `down --volumes` somente para apagar os dados da
demonstração. Para guardar uma cópia independente, crie e verifique um
checkpoint. [Backups e restauração](checkpoints.md)

Python é opcional. Para escrever uma aplicação em Python ou rodar os exemplos
da biblioteca, conecte o cliente a um servidor Bolt nativo ou em Docker.
[Exemplo de gravação e consulta em Python](getting-started.md#3-write-connected-data)

Para continuar, os guias técnicos estão em inglês.

- [Consultas Cypher e modo local](local-cypher.md)
- [Definição de nós e relações](online-catalog.md)
- [Importação e atualização de dados do DuckDB](duckdb-sync.md)
- [Exemplo de grafo de conhecimento com projetos e decisões](../examples/company_brain/README.md)
- [Recuperação de contexto e fontes para agentes](agent-context-quickstart.md)
- [Solução de problemas no Docker](container.md)
