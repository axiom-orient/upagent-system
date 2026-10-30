# 독립 ReviewCampaign

Provider-neutral review library와 별도 `agent-review` CLI의 제품이다. immutable subject를 독립 discovery·fact check·pure reducer로 검토하며 원래 Agent task나 workspace를 수정하지 않는다.

[정체성과 발전](docs/IDENTITY_AND_EVOLUTION.md) · [SPEC](docs/SPEC.md) · [ARCHITECTURE](docs/ARCHITECTURE.md) · [IMPLEMENTATION_STATUS](docs/IMPLEMENTATION_STATUS.md) · [PLAN](docs/PLAN.md).

Workspace root에서 `cargo build --locked --release -p agent-review-cli`로 빌드한다. request/route 파일과 실제 실행 조건은 [기존 ReviewCampaign 규범](../../docs/REVIEW_CAMPAIGN.md)을 따른다. packaged stale binary를 재사용하지 않는다.
