# WebhookDevModeStatusUpdatePayload

## Properties

Name | Type | Description | Notes
------------ | ------------- | ------------- | -------------
**passcode** | **String** | The passcode specified when the webhook was created, should match what was initially provided | 
**timestamp** | **String** | UTC ISO 8601 timestamp of when the event was triggered. | 
**webhook_id** | **String** | The id of the webhook that caused the callback | 
**event_type** | **String** |  | 
**file_key** | **String** | The key of the file that was updated | 
**file_name** | **String** | The name of the file that was updated | 
**node_id** | **String** | The id of the node where the Dev Mode status changed. For example, \"43:2\" | 
**related_links** | [**Vec<models::DevResource>**](DevResource.md) | An array of related links that have been applied to the layer in the file | 
**status** | **String** | The Dev Mode status. Either \"NONE\", \"READY_FOR_DEV\", or \"COMPLETED\" | 
**triggered_by** | [**models::User**](User.md) | The user that made the status change and triggered the event | 

[[Back to Model list]](../README.md#documentation-for-models) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to README]](../README.md)


