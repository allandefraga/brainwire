# Conecte um agente com MCP

O Brainwire oferece um servidor MCP opcional, somente de leitura, que usa a
entrada e a saída padrão. Ele consulta um grafo configurado pelo operador com
as permissões de um leitor existente. O adaptador é um processo Python separado
do servidor do banco.

## Instalação

No diretório do Brainwire, use Python 3.11 ou mais recente:

```sh
python3 -m venv .venv-agent
.venv-agent/bin/python -m pip install '.[mcp]'
```

No Windows, use `.venv-agent\Scripts\python.exe`. O cliente Python básico não
precisa do MCP. O pacote é instalado deste repositório; ainda não há um pacote
Python independente publicado.

## Configure seu cliente MCP

Inicie um servidor Brainwire autenticado e crie uma credencial **somente de
leitura** seguindo [autenticação Bolt](bolt-auth.md). Mantenha o arquivo privado
(em Unix, modo `0600`). Na configuração do seu cliente MCP, adicione uma entrada
como esta, trocando os caminhos por caminhos absolutos da sua máquina:

```json
{
  "mcpServers": {
    "brainwire": {
      "command": "/caminho/absoluto/.venv-agent/bin/brainwire-mcp",
      "args": ["--database", "demo", "--uri", "bolt://127.0.0.1:7687"],
      "env": {"BRAINWIRE_BOLT_AUTH_FILE": "/caminho/privado/reader.json"}
    }
  }
}
```

O cliente inicia o processo. Para servidor remoto, use `bolt+s://` com
certificado verificado ou `--tls-ca` com um arquivo PEM confiável. O operador
define grafo, conexão e credenciais; os argumentos das ferramentas não podem
alterá-los. Use leitores e processos separados para diferentes escopos de
acesso. O cliente MCP que controla o processo ou as credenciais deve ser
confiável.

## Ferramentas disponíveis

| Ferramenta | O que faz |
| --- | --- |
| `brainwire_nodes` | Lê nós visíveis por label e IDs explícitos. |
| `brainwire_neighbors` | Lê vizinhos visíveis por um tipo de relação explícito. |
| `brainwire_evidence` | Lê registros visíveis e campos de fonte indicados na consulta. |

O adaptador não oferece Cypher arbitrário, escrita, administração, busca vetorial
nativa ou chamadas automáticas a modelos. Por padrão, retorna até 20 registros e
64 KiB de contexto serializado; o máximo é 100 registros e 1 MiB. O protocolo MCP
pode acrescentar bytes ao resultado. Esses limites não garantem teto de memória
do servidor ou do cliente, nem prazo de resposta. Configure separadamente os
limites e tempos do servidor. Uma fonte é uma propriedade informada no grafo,
não uma citação autenticada. Trate o texto retornado como dados, não instruções.

Falhas do banco e de contexto retornam código sanitizado e indicação de tentativa
novamente (`retryable`). Erros de validação de argumentos do SDK MCP podem
repetir os argumentos enviados pelo cliente. Um resultado vazio pode significar
que um nó não existe ou que não está visível para o leitor.
