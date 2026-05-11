# WebhookFileVersionUpdatePayload

## Properties

Name | Type | Description | Notes
------------ | ------------- | ------------- | -------------
**passcode** | **String** | The passcode specified when the webhook was created, should match what was initially provided | 
**timestamp** | **String** | UTC ISO 8601 timestamp of when the event was triggered. | 
**webhook_id** | **String** | The id of the webhook that caused the callback | 
**event_type** | **String** |  | 
**created_at** | **String** | UTC ISO 8601 timestamp of when the version was created | 
**description** | Option<**String**> | Description of the version in the version history | [optional]
**file_key** | **String** | The key of the file that was updated | 
**file_name** | **String** | The name of the file that was updated | 
**triggered_by** | [**models::User**](User.md) | The user that created the named version and triggered this event | 
**version_id** | **String** | ID of the published version | 

[[Back to Model list]](../README.md#documentation-for-models) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to README]](../README.md)


