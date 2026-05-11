# WebhookLibraryPublishPayload

## Properties

Name | Type | Description | Notes
------------ | ------------- | ------------- | -------------
**passcode** | **String** | The passcode specified when the webhook was created, should match what was initially provided | 
**timestamp** | **String** | UTC ISO 8601 timestamp of when the event was triggered. | 
**webhook_id** | **String** | The id of the webhook that caused the callback | 
**event_type** | **String** |  | 
**created_components** | [**Vec<models::LibraryItemData>**](LibraryItemData.md) | Components that were created by the library publish | 
**created_styles** | [**Vec<models::LibraryItemData>**](LibraryItemData.md) | Styles that were created by the library publish | 
**created_variables** | [**Vec<models::LibraryItemData>**](LibraryItemData.md) | Variables that were created by the library publish | 
**modified_components** | [**Vec<models::LibraryItemData>**](LibraryItemData.md) | Components that were modified by the library publish | 
**modified_styles** | [**Vec<models::LibraryItemData>**](LibraryItemData.md) | Styles that were modified by the library publish | 
**modified_variables** | [**Vec<models::LibraryItemData>**](LibraryItemData.md) | Variables that were modified by the library publish | 
**deleted_components** | [**Vec<models::LibraryItemData>**](LibraryItemData.md) | Components that were deleted by the library publish | 
**deleted_styles** | [**Vec<models::LibraryItemData>**](LibraryItemData.md) | Styles that were deleted by the library publish | 
**deleted_variables** | [**Vec<models::LibraryItemData>**](LibraryItemData.md) | Variables that were deleted by the library publish | 
**description** | Option<**String**> | Description of the library publish | [optional]
**file_key** | **String** | The key of the file that was published | 
**file_name** | **String** | The name of the file that was published | 
**library_item** | [**models::LibraryItemData**](LibraryItemData.md) | The library item that was published | 
**triggered_by** | [**models::User**](User.md) | The user that published the library and triggered this event | 

[[Back to Model list]](../README.md#documentation-for-models) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to README]](../README.md)


