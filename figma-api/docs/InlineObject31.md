# InlineObject31

## Properties

Name | Type | Description | Notes
------------ | ------------- | ------------- | -------------
**rows** | [**Vec<models::AiUsageDailyRow>**](AiUsageDailyRow.md) | Per-user, per-day AI credit usage aggregates, ordered by `day`, then user, then `editor_type`. | 
**next_cursor** | **String** | An opaque cursor to pass as the `cursor` query parameter to fetch the next page. Empty when there are no more pages. | 
**has_next_page** | **bool** | Whether there is a next page of results to fetch. | 

[[Back to Model list]](../README.md#documentation-for-models) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to README]](../README.md)


