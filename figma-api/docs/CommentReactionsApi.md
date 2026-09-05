# \CommentReactionsApi

All URIs are relative to *https://api.figma.com*

Method | HTTP request | Description
------------- | ------------- | -------------
[**delete_comment_reaction**](CommentReactionsApi.md#delete_comment_reaction) | **DELETE** /v1/files/{file_key}/comments/{comment_id}/reactions | Delete a reaction
[**get_comment_reactions**](CommentReactionsApi.md#get_comment_reactions) | **GET** /v1/files/{file_key}/comments/{comment_id}/reactions | Get reactions for a comment
[**post_comment_reaction**](CommentReactionsApi.md#post_comment_reaction) | **POST** /v1/files/{file_key}/comments/{comment_id}/reactions | Add a reaction to a comment



## delete_comment_reaction

> models::InlineObject14 delete_comment_reaction(file_key, comment_id, emoji)
Delete a reaction

Deletes a specific comment reaction. Only the person who made the reaction is allowed to delete it.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | File to delete comment reaction from. This can be a file key or branch key. Use `GET /v1/files/:key` with the `branch_data` query param to get the branch key. | [required] |
**comment_id** | **String** | ID of comment to delete reaction from. | [required] |
**emoji** | **String** |  | [required] |

### Return type

[**models::InlineObject14**](inline_object_14.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_comment_reactions

> models::InlineObject15 get_comment_reactions(file_key, comment_id, cursor)
Get reactions for a comment

Gets a paginated list of reactions left on the comment.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | File to get comment containing reactions from. This can be a file key or branch key. Use `GET /v1/files/:key` with the `branch_data` query param to get the branch key. | [required] |
**comment_id** | **String** | ID of comment to get reactions from. | [required] |
**cursor** | Option<**String**> | Cursor for pagination, retrieved from the response of the previous call. |  |

### Return type

[**models::InlineObject15**](inline_object_15.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## post_comment_reaction

> models::InlineObject14 post_comment_reaction(file_key, comment_id, post_comment_reaction_request)
Add a reaction to a comment

Posts a new comment reaction on a file comment.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | File to post comment reactions to. This can be a file key or branch key. Use `GET /v1/files/:key` with the `branch_data` query param to get the branch key. | [required] |
**comment_id** | **String** | ID of comment to react to. | [required] |
**post_comment_reaction_request** | [**PostCommentReactionRequest**](PostCommentReactionRequest.md) | Reaction to post. | [required] |

### Return type

[**models::InlineObject14**](inline_object_14.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken)

### HTTP request headers

- **Content-Type**: application/json
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

