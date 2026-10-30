# ReviewCampaign — 아키텍처

```text
review CLI → request + selected generic provider routes
→ immutable subject materialization → review execution lease
→ discovery → frozen candidate findings → fact checks
→ pure reducer → durable result / recovery / repair claim
```

ReviewCampaign은 review state/evidence를 소유하고, agent-review-cli는 provider plugin setup·JSON I/O를 소유한다. ModelPort/ProviderBinding은 공통 public contract를 소비한다. 일반 Agent SessionMachine·ImageDirectorState와 별도 owner다.

상세 contract·lease·setup·failure 경계는 [ReviewCampaign 규범](../../../docs/REVIEW_CAMPAIGN.md)을 따른다. 이 문서는 실행 테스트 결과를 소유하지 않는다.
