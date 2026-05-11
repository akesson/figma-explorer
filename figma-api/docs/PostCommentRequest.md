# PostCommentRequest

## Properties

Name | Type | Description | Notes
------------ | ------------- | ------------- | -------------
**message** | **String** | The text contents of the comment to post. | 
**comment_id** | Option<**String**> | The ID of the comment to reply to, if any. This must be a root comment. You cannot reply to other replies (a comment that has a parent_id). | [optional]
**client_meta** | Option<[**models::PostCommentRequestClientMeta**](postComment_request_client_meta.md)> |  | [optional]

[[Back to Model list]](../README.md#documentation-for-models) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to README]](../README.md)


