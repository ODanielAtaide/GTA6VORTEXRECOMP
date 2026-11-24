# Vortex — Português (Brasil)

O Vortex é um projeto experimental de código aberto para pesquisa de compatibilidade Xbox Series com Windows x64. Trabalhamos com execução nativa x86-64, serviços de runtime, tradução gráfica Direct3D 12, recompilação de shaders e otimização de desempenho. Não é um port oficial nem uma recompilação completa de um jogo comercial.

Nossa meta inicial é **30 FPS com renderização correta**. O projeto preserva a autoria, o histórico e a licença dos projetos públicos VIctory e victory-gta6. Veja os [créditos originais](UPSTREAM_CREDITS.md).

## O que conseguimos até 24/11/2026

- Código-fonte editável na raiz do repositório e histórico upstream preservado.
- Compilação release no Windows concluída.
- 354 execuções de testes locais bem-sucedidas, incluindo um benchmark sintético, antes das alterações mais recentes da fila de texturas.
- Ferramentas de inspeção, métricas e comparação de desempenho.
- Correções de segurança de leitura de estruturas PE/TLS, otimizações de cópias de texturas, cache e agendamento.
- Registro preliminar de 30 FPS no menu e aproximadamente 0,8–1 FPS em uma cena 3D complexa, na máquina do mantenedor. A procedência da carga não foi verificada de forma independente e isso não representa um teste reproduzível nem uma garantia de compatibilidade.

O computador utilizado tem i5-9400, RX 6600 XT de 8 GB e 16 GB de RAM. Ainda há problemas de sincronização, cópias de memória, compilação de pipelines, texturas pendentes e renderização incompleta. Não foi medido o ganho das últimas alterações.

Os detalhes, commits e métricas estão no [relatório de desenvolvimento](DEVELOPMENT_STATUS.md) e no [roadmap](ROADMAP.md).

## Participe!

Buscamos pessoas com experiência em Rust, Direct3D 12, compiladores de shaders, GPUs, gerenciamento de memória, testes e documentação. Pequenas contribuições reproduzíveis também são importantes. Não é necessário possuir um jogo comercial para contribuir.

Abra uma [issue](https://github.com/ODanielAtaide/GTA6VORTEXRECOMP/issues/new), faça um fork ou envie um pull request seguindo o [guia de contribuição](../CONTRIBUTING.md). Utilize exemplos sintéticos próprios ou cargas autorizadas e não envie conteúdo proprietário, arquivos de jogos ou logs com informações privadas.

## Tecnologia e créditos

O Vortex é desenvolvido principalmente em **Rust**. Na fotografia do código de 24/11/2026, são 139 arquivos Rust (4.175.366 bytes) e seis arquivos Python (9.299 bytes). O Python é utilizado nas ferramentas auxiliares de diagnóstico e testes.

O mantenedor e responsável pelo Vortex é [@ODanielAtaide](https://github.com/ODanielAtaide). A lista automática de colaboradores do GitHub inclui autores do histórico importado; isso não significa que participaram diretamente do Vortex. Veja a separação de créditos em [AUTHORS.md](../AUTHORS.md).

A colaboração é aberta a todos. O projeto não tem vínculo oficial com Microsoft, Xbox, Rockstar Games ou Take-Two Interactive.

[README principal](../README.md) · [Licença](../LICENSE)
